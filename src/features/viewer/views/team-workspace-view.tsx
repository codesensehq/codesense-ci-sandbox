// SPDX-License-Identifier: MIT
import { FileNavSlotMount } from "@/features/shell/slots/file-nav-slot-host";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useRetiringFrames } from "@/features/dashboard/components/use-retiring-frames";
import { ScratchTreeBadge } from "@/features/viewer/components/scratch-file-badge";
import { usePrivateNavFiles } from "@/features/scratch-space/lib/use-private-nav-files";
import { Lock } from "lucide-react";
import { privateFile } from "@/shared/lib/private-file";
import { resolveFileSource } from "@/shared/lib/file-source";
import { useNavSource } from "@/shared/hooks/use-nav-source";
import { bridgeTiming, nowMs } from "@/features/dashboard/lib/bridge-timing";
import { useEffect, useLayoutEffect, useRef, useCallback, useState, useMemo } from "react";
import { createPortal } from "react-dom";
import { useTheme } from "next-themes";
import mermaid from "mermaid";
import { VIEWER_PERF } from "@/shared/lib/viewer-perf-config";
import { useContentSource } from "@/shared/hooks/use-content-source";
import { useTeamContentContext } from "@/shared/hooks/use-team-content-context";
import { useTeamPipelineRepoPath } from "@/shared/hooks/use-team-pipeline-repo-path";
import { useInstanceCount } from "@/shared/hooks/use-instance-count";
import { openEpicWorkspaceFor } from "../lib/epic-workspace-opener";
import { buildSyntheticRepoPathFromPrompt } from "@/shared/lib/build-synthetic-repo";
import { useActiveTeamKey, parseTeamKey } from "@/shared/lib/team-key";
import { OwningScopeProvider, isScopeActive } from "@/shared/lib/warm-scopes";
import { useOrgStore } from "@/shared/stores/org-store";
// SW-VE-5 / SW-XS-20: resolve the active tab's scope from the loaded scans and
// nav tree, to say where an inherited file's edits go.
import type { FileScope } from "@/features/dashboard/lib/file-scope";
import { copyIntoTeam } from "@/features/dashboard/lib/inherited-file";
import { activeFileScope, docToolbarGates } from "../lib/doc-toolbar-gates";
import {
  isShowingDiff,
  useViewerStore,
  promptIdentityFromRepoPath,
} from "@/shared/stores/viewer-store";
import type { WorkspaceLocationChoice } from "@/shared/lib/workspace-location";
import { OpenCliMenuItem } from "@/features/dashboard/components/status-nav/open-cli-menu-item";
import {
  useActiveWorkspaceId,
  useWorkspaceCount,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import { useUiStore } from "@/shared/stores/ui-store";
import { markContentPathChanged, useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { PipelineStatusNav } from "@/features/dashboard/components/pipeline-status-nav";
import { useContextMoveRowSlot } from "@/features/context-mobility/context-mobility-host";
import { useSprintRegistry } from "@/features/viewer/hooks/use-sprint-registry";
import { LeftNavHeader } from "@/features/dashboard/components/left-nav-header";
import { findScopedSubtree } from "@/shared/lib/pipeline-graph/pipeline-utils";
import { useStrandedDrillFallback } from "../hooks/use-stranded-drill-fallback";
import { buildPipelineGraph } from "@/shared/lib/pipeline-graph/pipeline-graph";
import type { DerivedPipelineTree, PipelineStatus } from "@/shared/lib/pipeline-graph/types";
import { renderViewer } from "@/shared/lib/render-viewer";
import { useWorktreeRosterStore } from "../stores/worktree-roster-store";
import { FileViewerToolbar } from "../components/file-viewer-toolbar";
import type { TabDiff } from "@/shared/stores/viewer-store";
import { useContextGit } from "@/features/git-sync/store/use-context-git-store";
import { isVirtualTabPath } from "@/shared/lib/virtual-viewer-registry";
import { fileViewKey } from "@/shared/lib/view-snapshot";
import { isPreviewableFilePath, isViewerFilePath } from "@/shared/lib/viewer-file-type";
import { FileWarningPlaceholder } from "@/features/viewer/components/file-warning-placeholder";
import { derivedTreeToReactFlow } from "@/shared/lib/pipeline-graph/derived-tree-to-react-flow";
import { type RelateDocActions } from "../components/related-docs-chip";
import { relateDoc, unrelateDoc } from "@/features/dashboard/lib/relate-doc";
import { collectRelateCandidates } from "@/features/dashboard/lib/relate-candidates";
import { computeDocRelations, shouldShowRelatedChip } from "../lib/doc-relations";
import { ContextEditorInset } from "../components/context-editor-inset";
import { MergeMarkedViewer } from "../components/merge-marked-viewer";
import { MetadataBand } from "../components/metadata-band";
import { isReadOnlyLayeredDoc } from "../lib/is-read-only-layered-doc";
import { listSourceReposForTeam, type SourceRepoOption } from "@/tauri/commands";
import { findMetadataValidity } from "@/shared/lib/pipeline-graph/find-metadata-validity";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { exportViewerToPdf } from "../lib/export-pdf";
import { FindBar } from "../components/find-bar";
import { useWorkspaceBarCallbacks } from "../hooks/use-workspace-bar-callbacks";
import { useFocusActiveWorkspaceDoc } from "../hooks/use-focus-active-workspace-doc";
import { toastError, withErrorToast } from "@/shared/lib/with-error-toast";
import { WorktreeSetupApprovalModal } from "../components/worktree-setup-approval-modal";
import { NoLocalCloneModal } from "../components/no-local-clone-modal";
import { QuickOpenDialog } from "../components/quick-open-dialog";
import { handleQuickOpenFileSelect } from "../lib/quick-open-select";
import { useTeamWorkspaces } from "@/shared/hooks/use-team-workspaces";
import { useGesturePaneScope } from "@/shared/hooks/use-gesture-pane-scope";
import {
  focusScratchNoteEditor,
  focusVisibleTerminal,
  focusViewerContent,
  toggleRightPaneWithFocus,
  toggleNavCollapsed,
} from "../lib/focus-cascade";
import { buildWorkspacePickerItems, decodePickerId } from "../lib/workspace-picker-items";
import { NoteSwitcherMount } from "../components/note-switcher-mount";
import { HeadingJumpMount } from "../components/heading-jump-mount";
import { openGenericWorkspace } from "../lib/open-generic-workspace";
import { getActivePaneType, isRowPopupFocused, type PaneType } from "@/shared/lib/focus-utils";
import { readFile, type ContextFile } from "@/tauri/commands";
import { writePipelineStatus } from "@/shared/lib/pipeline-status-writer";
import { fileContentCache } from "@/shared/lib/file-content-cache";
import {
  CONTENT_SCAN_TIMEOUT_MS,
  fileListErrorMessage,
  isNotIndexedError,
  requestContentFileIndex,
} from "@/shared/lib/pipeline-graph/content-scan-queue";
import { quickOpenContentFiles } from "@/shared/lib/pipeline-graph/quick-open-content-files";
import { toast } from "sonner";
import { RightPaneLayout } from "../components/right-pane-layout";
import { StoryAgentLaunchProvider } from "@/features/story-launch/use-story-agent-launch";
import { usePipelineTree } from "@/shared/lib/pipeline-graph/use-pipeline-tree";
import { publishTeamTree } from "../lib/team-surface-store";
import { useRightPaneHost } from "../lib/right-pane-host-registry";
// SLW-NC-1: source-repo content merged into the NAV tree only. SW-XS-15: it is the
// repo-tier files of the selected agent's want set. Via the pipeline-graph barrel.
import {
  useTeamRepoGlyphMap,
  wantSetToRepoScan,
  mergeInstalledIntoNav,
  isInstalledNavPath,
} from "@/shared/lib/pipeline-graph";
import { useAgentContext } from "@/features/agent-context/hooks/use-agent-context";
import {
  navRelResolver,
  repoGroupName,
  repoTierFiles,
  teamRelResolver,
  type NavAgentContext,
} from "@/features/agent-context/lib/nav-agent-context";
import { useIsLightViewerTheme } from "@/shared/lib/viewer-theme";
import { resolveContentSource } from "@/shared/lib/resolve-content-source";

import { isStoryFile as isStoryFilePredicate } from "@/shared/lib/worktree";
import { getEffectiveTreeFontPx } from "@/shared/lib/font-sizes";
import { useTrailingDebounce } from "@/shared/lib/use-trailing-debounce";
import { PaneCollapseDivider, type PaneSize } from "../components/pane-collapse-divider";
import type { DragResizeOptions } from "./use-drag-resize";

const EMPTY_PATHS: string[] = [];

/** The nav's width range: History reads as columns past 640px, so on a big
 *  enough window the nav may be dragged (or arrowed) that wide. */
const NAV_MIN_WIDTH_PX = 150;
function navMaxWidthPx(): number {
  return Math.max(500, Math.round((typeof window !== "undefined" ? window.innerWidth : 0) * 0.6));
}

interface TeamWorkspaceViewProps {
  teamKey: string;
}

export function TeamWorkspaceView({ teamKey }: TeamWorkspaceViewProps) {
  // RF-perf-4 (AC-1): count this mounted team subtree for the gated memory line.
  // RF-perf-10 unmounts hidden teams (active ∨ dirty ∨ retained), so this tracks
  // exactly the kept-mounted team count that gates all the sub-surface counts.
  useInstanceCount("team");
  // T.6 (Task 8): the component is now keyed by team — one mount per
  // opened team in the shell's workbench. `repoPath` is the synthetic workspace key
  // (`workspace:{org}:{team}:{owner}/{repo}`) for the active pill —
  // sourced from `useActiveWorkspaceContext().repoPath`, which derives
  // it from `(teamKey, workspace.sourceRepoSlug)`. Pipeline tree, file
  // refresh, settings, and viewer content all track whichever pill
  // (root or story) is focused in this team's workspace bar.
  const workspaces = useTeamWorkspaces(teamKey);
  const activeWorkspaceId = useActiveWorkspaceId(teamKey);
  const activeWorkspaceObj = useMemo(
    () => workspaces.find((w) => w.id === activeWorkspaceId) ?? null,
    [workspaces, activeWorkspaceId],
  );
  const repoPath = useMemo(() => {
    const slug = activeWorkspaceObj?.sourceRepoSlug;
    if (!slug) return "";
    return buildSyntheticRepoPathFromPrompt({ teamKey, sourceRepoSlug: slug }) ?? "";
  }, [activeWorkspaceObj?.sourceRepoSlug, teamKey]);
  // SW-EW-24: the repoPath whose pipeline tree feeds the left nav + viewer tab
  // bar. For a normal workspace this equals `repoPath` (same own-slug
  // derivation); for an epic *integration* shell workspace — which has no
  // `sourceRepoSlug`, so identity `repoPath` is "" — it falls back to the
  // team's first source repo so the nav + tabs render the team pipeline
  // instead of blanking. Identity `repoPath` is deliberately left "" for an
  // epic workspace (terminal rooting, data-repo-path, file-refresh keys all
  // key off it); only the nav-data chain below reads `navRepoPath`.
  const teamContent = useTeamContentContext(teamKey);
  const navRepoPath = useTeamPipelineRepoPath(activeWorkspaceObj, teamKey);
  // The context repo's org_root: the redline's root and the metadata commands' (null until
  // resolved).
  const navSource = useNavSource(teamKey, navRepoPath);
  const cloneOrgRoot = navSource.cloneRoot;
  const isPrivate = navSource.source.kind === "private";
  const orgRoot = isPrivate ? navSource.orgRoot : cloneOrgRoot;
  const repo = useContentSource(navRepoPath || null);

  const orgs = useOrgStore((s) => s.orgs);
  // SLW-PT-18: the per-file ⋯ move/copy/remove affordance, wired to the active
  // pivot context as the SOURCE + the app-shell org/create-team seams.
  // Built here (in the full app context) and handed to the nav as a render-prop,
  // so the nav stays decoupled from the pivot stores.
  const moveActionSlot = useContextMoveRowSlot(teamKey, navRepoPath);
  const repoName = repo?.repoName;
  const rootDisplayName = repoName;

  // T.4 Phase 5: file-tab slices are team-keyed; use `teamKey`.
  const activeTabPath = useViewerStore((s) => s.activeTab[teamKey]) ?? "";
  const privateFiles = usePrivateNavFiles(
    teamKey,
    isPrivate
      ? (navSource.content?.teamContentPath ?? null)
      : (privateFile(activeTabPath)?.contentRoot ?? null),
  );
  // Tabs showing their Changes (the file against GitHub) instead of the file.
  const tabDiff = useViewerStore((s) => s.tabDiff[teamKey]);
  const changeByPath = useContextGit(teamKey).byPath;
  useWorktreeRosterStore((s) => s.rows);
  const activeSourceMatches =
    resolveFileSource(activeTabPath).source.kind === navSource.source.kind;
  const activeDiff =
    activeSourceMatches && !!activeTabPath && isShowingDiff(tabDiff?.[activeTabPath]) && !!orgRoot;
  // Scoped trailing debounce for FindBar's `containerKey` prop only.
  // FindBar re-runs a `document.createTreeWalker` highlight + smooth-
  // scrolled `scrollIntoView` whenever `containerKey` changes; rapid
  // Cmd+Shift+] cycling with an active query would compound those into
  // a visible stutter on every intermediate tab. The rest of the
  // tab-switch path commits synchronously (32.6 made it cheap), so this
  // debounce is narrow: FindBar-only, ~50ms (safely above macOS ~30ms
  // autorepeat cadence, under user-perceptible threshold for single
  // presses).
  const findContainerKey = useTrailingDebounce(activeTabPath, 50);
  const sidebarWidth = useViewerStore((s) => s.settings?.sidebarWidth) ?? 240;
  const navCollapsed = useViewerStore((s) => s.navCollapsed[teamKey] ?? false);
  // SW-EW-33: the left workspace panel (collapse/width state) is retired; its
  // persisted fields are left inert (D4). The tray replaces it.
  const treeFontSize = useViewerStore((s) => s.settings?.treeFontSize) ?? "large";
  const treeFontSizePx = useViewerStore((s) => s.settings?.treeFontSizePx);
  const treeFontPx = getEffectiveTreeFontPx(treeFontSize, treeFontSizePx);

  // SR-9: the read-only viewer + editor inset key on `contentRefreshKeys` (the
  // viewer content-version), NOT `fileRefreshKeys` (the pipeline-scan counter). A
  // `ContentLayerEvent` materialize storm counter-only bumps `fileRefreshKeys` ~1×/sec
  // for an open, unchanged file; keying the viewer on `contentRefreshKeys` — which a
  // counter-only bump never advances — makes that storm a no-op (0 re-reads, 0
  // re-renders) while pipeline/nav consumers still see the scan tick below.
  const contentRefreshKeys = useFileRefreshStore((s) => s.contentRefreshKeys);
  const lastChangedContextFiles = useFileRefreshStore((s) => s.lastChangedContextFile);
  // Navigation and document reads share the selected team's content identity.
  // Scan ticks include counter-only waves; document reads use the narrower
  // content refresh counter above.
  const navRefreshPath = isPrivate ? (navSource.privateSource?.contentPath ?? "") : navRepoPath;
  const navRefreshKey = useFileRefreshStore((s) => s.fileRefreshKeys[navRefreshPath]) ?? 0;

  const isActiveView = useActiveTeamKey() === teamKey;
  // F.4: single full-tree scan (scope="") shared by the nav, the viewer tabs
  // and the right pane (scoped views derived client-side).
  const {
    tree: fullPipelineTree,
    loading: fullTreeLoading,
    error: fullTreeError,
    refetch: refetchTree,
  } = usePipelineTree(navRepoPath, "", undefined, isActiveView, teamKey);
  // The shell's surfaces for the active team (the shared tray, the epic repos overlay) read this
  // one scan. Published before paint, so a switch draws the team's grouping in its first frame.
  useLayoutEffect(() => {
    if (!fullTreeLoading && fullPipelineTree) publishTeamTree(teamKey, fullPipelineTree);
  }, [teamKey, fullPipelineTree, fullTreeLoading]);
  // W.8 + R-18: reconcile workspace frontmatter fields (storyStatus, storyId,
  // sourceRepoSlug) against the latest pipeline tree.
  // R-18 Phase B (AC #27): also re-fire whenever the workspace count for
  // this repo changes — closes the dep-array gap where a workspace added
  // after the tree had resolved would otherwise never sync (its
  // `storyStatus` would stay undefined → no status icon). Length is the
  // right granularity: adds/removes flip it, but in-place reconciliation
  // (sync's own writes) does not — so no infinite-loop risk.
  const syncFrontmatter = useViewerStore((s) => s.syncWorkspaceFrontmatter);
  const workspaceCount = useWorkspaceCount(teamKey);
  useEffect(() => {
    if (!fullPipelineTree) return;
    syncFrontmatter(teamKey, fullPipelineTree);
  }, [fullPipelineTree, teamKey, syncFrontmatter, workspaceCount]);

  // N.1: derive the scoped pipeline tree for the status nav.
  // When drilled into a sub-epic, show only that subtree.
  // T.4 Phase 5: pipelineScope is team-keyed.
  const pipelineScope = useViewerStore((s) => s.pipelineScope[teamKey] ?? "");
  const privateTree = usePipelineTree(
    navSource.privateSource?.contentPath ?? "",
    "",
    navSource.content,
    isActiveView && isPrivate,
    teamKey,
  );
  const selectedFullTree = isPrivate ? privateTree.tree : fullPipelineTree;
  const scopedPipelineTree = useMemo(() => {
    if (!selectedFullTree) return null;
    return findScopedSubtree(selectedFullTree, pipelineScope);
  }, [selectedFullTree, pipelineScope]);

  // SW-FX-1: full-graph readiness seam, built once from the full tree (NOT the
  // drilled subtree). The nav renders the scoped tree but resolves readiness
  // through this, so a story's readiness can't flip on drill.
  const pipelineGraph = useMemo(
    () => (fullPipelineTree ? buildPipelineGraph(fullPipelineTree) : null),
    [fullPipelineTree],
  );

  // SW-XS-15: the left-nav context pivot — "Context for ‹Agent› · in ‹Repo›" or Team
  // (everything). One instance per team view, shared by the header, the nav overlay and
  // the nav's context menu. Only the active team resolves (a backgrounded view is idle).
  const agentContext = useAgentContext(teamKey, useActiveTeamKey() === teamKey);

  // SLW-NC-1 / SW-XS-15: with a repo picked, the nav also shows the files the agent gets
  // from that repo's own content tier (`repos/{o}/{r}/content/`), merged flat into the
  // team tree. Resolved by the pivot's one want-set read, so it never blocks the nav.
  const selectedSourceRepo = agentContext.repo
    ? `${agentContext.repo.owner}/${agentContext.repo.repo}`
    : "";
  const installedScans = useMemo(
    () =>
      agentContext.view && selectedSourceRepo
        ? [
            wantSetToRepoScan(
              selectedSourceRepo,
              repoTierFiles(agentContext.view),
              repoGroupName(agentContext.repo?.repo ?? selectedSourceRepo),
            ),
          ]
        : [],
    [agentContext.view, agentContext.repo, selectedSourceRepo],
  );
  // The provenance glyph map shared by the pivot and the nav's repo tint (P2-AC-3).
  const sourceRepoGlyphMap = useTeamRepoGlyphMap(teamKey);

  // SLW-NC-1: the nav's flatten input = context scoped tree + the selected repo's want-set
  // files (root scope only). `fullPipelineTree` and everything it feeds (the readiness graph
  // above, `syncFrontmatter`, the workspace tray) stay pure context — so AC-4 (graph
  // unaffected) / AC-5 (no context-nav regression) hold by construction.
  const fileProjectionTree = useMemo(
    () => mergeInstalledIntoNav(fullPipelineTree, "", installedScans, selectedSourceRepo),
    [fullPipelineTree, installedScans, selectedSourceRepo],
  );
  const navTree = useMemo(
    () =>
      isPrivate
        ? scopedPipelineTree
        : mergeInstalledIntoNav(
            scopedPipelineTree,
            pipelineScope,
            installedScans,
            selectedSourceRepo,
          ),
    [scopedPipelineTree, pipelineScope, installedScans, selectedSourceRepo, isPrivate],
  );

  // The left nav is a stable folder tree — sprint filtering lives in the board,
  // not here (SW-SB-19's nav filter was removed: nav = place, board = work view).
  // The nav still needs the sprint registry for the SW-SB-15 "add to sprint"
  // right-click menu, so load it here and pass it down.
  const { registry: navSprintRegistry } = useSprintRegistry(teamKey);

  /** N.1: write status to frontmatter and trigger a pipeline rescan. */
  const handleNavStatusChange = useCallback(
    async (filePath: string, newStatus: PipelineStatus) => {
      try {
        // Serialized per file (shared with the board + card writers) so rapid
        // concurrent status changes to the same story land in call order.
        await writePipelineStatus(filePath, newStatus);
        // SW-EW-24: bump the repo whose tree the nav renders (`navRepoPath`).
        markContentPathChanged(navRepoPath, filePath);
      } catch (err: unknown) {
        toastError("Failed to update status: ", err);
      }
    },
    [navRepoPath],
  );

  const sidebarRef = useRef<HTMLDivElement>(null);
  const viewerContentRef = useRef<HTMLDivElement>(null);
  const [findVisible, setFindVisible] = useState(false);
  // Right-click target for the viewer context menu. `null` = background
  // click (menu shows "Find in document"); non-null = user right-clicked
  // an internal `.md` link (menu shows "Open in new tab" + "Copy link").
  const [contextMenuTarget, setContextMenuTarget] = useState<{
    external?: boolean;
    path: string;
    hash?: string;
  } | null>(null);
  // Store-owned fragments survive cross-team opens before this view is mounted.
  const pendingHash = useViewerStore((state) => state.fileHashRequests?.[teamKey] ?? null);
  // Post-commit content-version counter. Bumped by the active
  // `MarkdownViewer`'s `onContentCommitted` callback after React commits
  // new `blocks` / `hast` / `content`. Distinct from `contentRefreshKey`
  // (pre-parse intent) because the DOM actually lands async. FindBar
  // uses this as its `contentVersion` dep so re-highlight happens
  // against the new DOM, not the stale one. See story 32.7 root-cause
  // notes for Fix 3.
  const [viewerContentVersion, setViewerContentVersion] = useState(0);

  const bumpViewerContentVersion = useCallback(() => {
    setViewerContentVersion((v) => v + 1);
  }, []);
  const handlePendingHashConsumed = useCallback(() => {
    if (pendingHash) useViewerStore.getState().consumeFileHash(teamKey, pendingHash);
  }, [teamKey, pendingHash]);
  // The workbench's right-column host for this team's right pane.
  const rightPaneHost = useRightPaneHost(teamKey);

  const focusTerminal = () => requestAnimationFrame(focusVisibleTerminal);

  // The registry's `nav.quick-open` (Cmd+P) opens it; only the active view renders it.
  const quickOpenVisible = useUiStore((s) => s.fileQuickOpenOpen);
  const setQuickOpenVisible = useUiStore((s) => s.setFileQuickOpenOpen);
  const [quickOpenFiles, setQuickOpenFiles] = useState<ContextFile[]>([]);
  const [quickOpenError, setQuickOpenError] = useState<string | null>(null);
  const [quickOpenLoading, setQuickOpenLoading] = useState(false);
  const quickOpenRoot =
    (isPrivate ? navSource.content?.teamContentPath : repo?.teamContentPath) ?? "";
  const effectiveQuickOpenFiles = useMemo(
    () => quickOpenContentFiles(quickOpenFiles, isPrivate ? privateTree.tree : fullPipelineTree),
    [quickOpenFiles, isPrivate, privateTree.tree, fullPipelineTree],
  );
  // SW-WT-9: team-sync setup provenance gate. Scoped to this team's view so
  // only one modal renders.
  const worktreeSetupApproval = useViewerStore((s) => s.worktreeSetupApproval);
  const resolveWorktreeSetupApproval = useViewerStore((s) => s.resolveWorktreeSetupApproval);
  const setupApprovalRequest = useMemo(() => {
    if (!worktreeSetupApproval || worktreeSetupApproval.teamKey !== teamKey) return null;
    return { steps: worktreeSetupApproval.steps, repoLabel: worktreeSetupApproval.repoLabel };
  }, [worktreeSetupApproval, teamKey]);

  const workspaceBarCallbacks = useWorkspaceBarCallbacks(teamKey);

  // SW-CW-5: when the opt-in setting is on, focus the active workspace's own doc
  // (epic README / story file) in the viewer on activation, so switching
  // workspaces never leaves a prior workspace's doc showing. Default off. The
  // activation is not a request to read the doc, so it keeps full-screen CLI on.
  const focusWorkspaceDocOnActivation = useViewerStore(
    (s) => s.settings.focusWorkspaceDocOnActivation ?? false,
  );
  const focusActiveWorkspaceDoc = useCallback(
    (storyFilePath: string) =>
      useViewerStore.getState().openStoryDocForTeam(teamKey, storyFilePath, { keepLayout: true }),
    [teamKey],
  );
  useFocusActiveWorkspaceDoc(
    activeWorkspaceObj,
    focusActiveWorkspaceDoc,
    focusWorkspaceDocOnActivation,
  );

  const isLightTheme = useIsLightViewerTheme();
  // SLW-NC-1 Phase 2 (P2-AC-3): the installed-group provenance badge inputs,
  // bundled + memoized so `PipelineStatusNav`'s row-callbacks memo (and the
  // memoized `FolderHeaderRow`) see a stable identity across renders.
  const navProvenance = useMemo(
    () => ({
      glyphMap: sourceRepoGlyphMap,
      isLightTheme,
    }),
    [sourceRepoGlyphMap, isLightTheme],
  );
  // The single "open a story workspace by direct path + toast on error" path,
  // shared by the ⇧⌘K quick-switch picker, the nav's "Open story workspace",
  // and the file quick-open dialog.
  // T.5: entry point takes `teamKey` so the workspace lands in the active
  // team's bar. T-13: no `expectedSlug` — direct-path opens have no scan-time
  // slug to compare against (only the pipeline view captures one at scan time).
  const openStoryWorkspaceByPath = useCallback(
    (filePath: string, location?: WorkspaceLocationChoice) => {
      withErrorToast(
        useViewerStore
          .getState()
          .openOrFocusStoryWorkspace(
            teamKey,
            filePath,
            undefined,
            undefined,
            undefined,
            true,
            ...(location ? [location] : []),
          ),
        "Couldn't open story workspace: ",
      );
    },
    [teamKey],
  );
  // The story row's "Open CLI": where it lives, or (new) This Mac or a team machine.
  const renderOpenCli = useCallback(
    (filePath: string, onClose: () => void) => (
      <OpenCliMenuItem
        teamKey={teamKey}
        storyFilePath={filePath}
        onOpen={openStoryWorkspaceByPath}
        onClose={onClose}
      />
    ),
    [teamKey, openStoryWorkspaceByPath],
  );
  // SW-EW-6: nav "Open epic terminal" — open the epic WORKSPACE. Always
  // ensures+syncs the epic dir (`_epic/{id}/`) and opens a terminal there, even
  // for a draft epic with no `repos:` yet (you configure repos from the node).
  // SW-EW-9: the org/team/home context every epic command shares. Returns null
  // (caller decides whether to toast) when the active org/team isn't fully
  // resolved — the shared `resolveEpicCommandContext` (SW-DX-15) is the ONE
  // resolver, used by both the epic-open path and the repo-set handler below.
  // SW-DX-15: the epic-open orchestration + its 15-dep wiring now live in the
  // shared `epic-workspace-opener` module (the `epic` arm of `openWorkspace`),
  // so BOTH this nav handler and the dashboard `openWorkspace` verb call the same
  // unit. The nav supplies the target from the active team; the honest-degrade
  // toast on missing context moved into `openEpicWorkspaceFor`.
  const handleOpenEpicTerminal = useCallback(
    async (epicDirName: string, epicFilePath: string) => {
      const { orgSlug, teamName } = parseTeamKey(teamKey);
      // A null/`_none` team resolves to a null command context inside
      // `openEpicWorkspaceFor` → the same honest-degrade toast the old handler had.
      await openEpicWorkspaceFor({
        teamKey,
        orgSlug,
        team: teamName ?? "_none",
        epicDirName,
        epicFilePath,
      });
    },
    [teamKey],
  );
  // SW-EW-2 AC-5: dispatch a quick-switch pick to the right open verb. Items
  // are open workspaces + tree stories only — no epic-as-workspace.
  const handleWorkspacePickerSelect = useCallback(
    (id: string) => {
      const action = decodePickerId(id);
      if (!action) return;
      if (action.kind === "workspace") workspaceBarCallbacks.onSelectWorkspace(action.workspaceId);
      else if (action.kind === "generic") void openGenericWorkspace();
      else openStoryWorkspaceByPath(action.filePath);
    },
    [workspaceBarCallbacks, openStoryWorkspaceByPath],
  );
  // SW-EW-2 AC-5: quick-switch picker over every workspace target in the team.
  // SW-EW-33: open-state lives in ui-store (`workspacePaletteOpen`) so the
  // registry shortcut `nav.open-workspace` (Cmd+K, SW-FX-49) can open it; the
  // launcher + empty-state button open it the same way. Only the ACTIVE team's
  // view renders the dialog (the flag is global, the items are team-scoped).
  const workspacePaletteOpen = useUiStore((s) => s.workspacePaletteOpen);
  const setWorkspacePaletteOpen = useUiStore((s) => s.setWorkspacePaletteOpen);
  // SW-SC-10: scope Excalidraw's document-level trackpad-pinch zoom to the
  // pane under the pointer, so pinching over the pipeline can't zoom the
  // canvas. Idempotent across the team views (the decision is global).
  useGesturePaneScope();

  const noClonePromptRepo = useViewerStore((s) => s.noClonePromptRepo);
  const dismissNoClonePrompt = useViewerStore((s) => s.dismissNoClonePrompt);
  // viewIdentity drives the no-clone modal's teamKey match below. (SW-WT-1
  // removed the worktree "setup required" modal — a missing worktree.yaml is
  // now a calm, non-blocking state, not a wall.)
  const viewIdentity = useMemo(() => promptIdentityFromRepoPath(repoPath), [repoPath]);

  const drillPath = useViewerStore((s) => s.drillPaths[teamKey]);

  // SW-FX-8: the divider click and the hotkey route through the SAME shared
  // helpers (focus-cascade) so the two surfaces can never drift.
  const toggleNav = useCallback(() => toggleNavCollapsed(teamKey), [teamKey]);

  // SW-FX-8: nav resize wired through useDragResize (via PaneCollapseDivider),
  // replacing the prior hand-rolled mouse handlers. Pixel-based (150–500px),
  // unlike the ratio-based terminal/scratch dividers, so computeRatio returns
  // the px width directly and getContainerSize is a non-zero stand-in (the hook
  // only uses it to gate a zero-size abort).
  // Delta-based (committed width + how far the cursor moved), NOT absolute
  // clientX: the divider's cursor/hit zone is ~15px wide, so grabbing anywhere
  // but the exact 1px hairline with an absolute compute would SNAP the edge to
  // the cursor the instant the drag engages. Tracking the delta keeps the pane
  // under the grab point, matching the ratio dividers.
  const navResize = useMemo<Omit<DragResizeOptions, "onTap">>(
    () => ({
      getContainerSize: () => (typeof window !== "undefined" ? window.innerWidth : 1),
      getStartRatio: () => useViewerStore.getState().settings?.sidebarWidth ?? 240,
      getPos: (e) => e.clientX,
      computeRatio: ({ startRatio, startPos, currentPos }) =>
        Math.min(Math.max(startRatio + (currentPos - startPos), NAV_MIN_WIDTH_PX), navMaxWidthPx()),
      // SLW-FX-9: the global `.dragging` class and the window-wide `col-resize`
      // cursor are now owned by useDragResize (`dragCursor`, via
      // PaneCollapseDivider edge="left"), which clears them on EVERY teardown
      // path. The nav's former hand-rolled onDragStart/onCleanup that set/cleared
      // `document.body.style.cursor` + `.dragging` were the reference pattern for
      // this fix and are now redundant — removed so all dividers share one seam.
      onVisualUpdate: (w) => {
        if (sidebarRef.current) sidebarRef.current.style.width = `${w}px`;
      },
      onCommit: (w) => useViewerStore.getState().updateSettings({ sidebarWidth: w }),
    }),
    [],
  );

  // The keyboard half of the same divider: arrows step the width within the drag's range.
  const navMaxPx = navMaxWidthPx();
  const navSize = useMemo<PaneSize>(
    () => ({
      now: sidebarWidth,
      min: NAV_MIN_WIDTH_PX,
      max: navMaxPx,
      step: 8,
      largeStep: 32,
      format: (w) => `${w} pixels`,
      // The drag value is already the width in px.
      fromDrag: (w) => w,
      // Re-clamp to the live window: the render-time max may predate a resize.
      onChange: (w) =>
        useViewerStore.getState().updateSettings({ sidebarWidth: Math.min(w, navMaxWidthPx()) }),
    }),
    [sidebarWidth, navMaxPx],
  );

  const workspacePickerItems = useMemo(
    () => buildWorkspacePickerItems(workspaces, fullPipelineTree),
    [workspaces, fullPipelineTree],
  );

  // Cmd+F to open find bar, Ctrl+T to toggle terminal
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      // T.6 (Task 8): gate handlers on the active team. Only the active
      // team's keystrokes drive viewer-scoped actions; other mounted
      // teams (display:none) are inert.
      if (!isScopeActive(teamKey)) return;
      // The full-screen inbox covers the viewer: its keys (find, quick-open, tab and
      // pane cycling) would act out of sight, as the shared shortcut gate rules too.
      // A row popup of the Agents pane (the quick view, the inbox chat) is not the viewer or a CLI
      // tab: find, pane cycling and tab cycling do nothing there.
      const inPopup = isRowPopupFocused();

      if ((e.metaKey || e.ctrlKey) && e.key === "f" && !inPopup) {
        e.preventDefault();
        // SW-VE-8: single-owner ⌘F. When a SEARCH-CAPABLE CM6 editor holds focus
        // (the deliberate-edit inset — marked `data-cm-search`) its own in-editor
        // find (searchKeymap) owns ⌘F — do NOT also open the viewer FindBar (two
        // find UIs from one keypress). CM6's keymap preventDefaults but doesn't
        // stopPropagation, so this document-level handler must bow out itself
        // Gated on the marker,
        // NOT a bare `.cm-editor`, so the READ-ONLY code viewer (focusable, no
        // search extension) still gets the viewer FindBar.
        if ((document.activeElement as HTMLElement | null)?.closest?.("[data-cm-search]")) return;
        setFindVisible(true);
      }
      // SW-EW-33: ⇧⌘K (open/switch workspace palette) moved to the shortcut
      // registry as `nav.open-workspace` (discoverable + rebindable). It now
      // opens via `ui-store.workspacePaletteOpen`; the handler below is gone.
      // Cmd+` — cycle focus through visible panes: viewer → terminal → scratch note
      if (e.metaKey && e.key === "`" && !inPopup) {
        e.preventDefault();
        const state = useViewerStore.getState();
        const mode = state.layoutMode ?? "shared";
        const noteOpen = state.rightPaneOpen;

        // Build ordered list of visible panes
        const visiblePanes: PaneType[] = [];
        if (mode !== "console") visiblePanes.push("file");
        if (mode === "console" || mode === "shared") visiblePanes.push("terminal");
        if (noteOpen) visiblePanes.push("scratch-note");

        // If 0 or 1 visible panes, open terminal as fallback
        if (visiblePanes.length <= 1 && !visiblePanes.includes("terminal")) {
          state.setLayoutMode(state.lastTerminalMode);
          focusTerminal();
          return;
        }
        if (visiblePanes.length <= 1) return;

        // Find current pane and advance to next
        const current = getActivePaneType();
        const idx = visiblePanes.indexOf(current);
        const nextPane = visiblePanes[(idx + 1) % visiblePanes.length];

        // Focus the next pane
        if (nextPane === "terminal") {
          focusTerminal();
        } else if (nextPane === "scratch-note") {
          requestAnimationFrame(focusScratchNoteEditor);
        } else {
          requestAnimationFrame(focusViewerContent);
        }
      }
      // Cmd+\ (toggle terminal), Cmd+J (toggle scratch note), and Cmd+Shift+\
      // (toggle console ↔ shared, SW-EW-35 `viewer.toggle-console`) are now
      // dispatched by `useShortcutHandler` via the canonical action registry —
      // they were moved there so users can rebind them.
      //
      // Cmd+Shift+E — now handled by the shortcut registry (viewer.export-pdf)
      // Cmd+T (new terminal tab in the focused tile) is the registry's
      // `viewer.new-terminal-tab`.
      // Cmd+P (file quick-open) is the registry's `nav.quick-open`.
      // SW-VE-8: ⌘E (enter deliberate edit) moved to the canonical shortcut
      // registry (`viewer.edit-file`) — discoverable in Settings → Shortcuts +
      // the File menu and user-rebindable. Its `run()` drives the viewer store
      // (`requestEditActiveFile`), which the effect below honors through the
      // same `editEntryRef` gate this inline branch used. No local keydown here.
      // Cmd+Shift+] / Cmd+Shift+[ switch tabs (unshifted, they are the registry's team and
      // workspace back and forward). Excludes Alt so `Cmd+Opt+]` / `Cmd+Opt+[` (workspace-cycle
      // in the canonical shortcut registry, story 30.10 AC-19) doesn't race against this
      // handler and cycle a terminal tab instead.
      // Skip when a modal (quick-open, find bar) is open — don't cycle tabs behind it
      if (
        !e.defaultPrevented &&
        e.metaKey &&
        e.shiftKey &&
        !e.altKey &&
        getActivePaneType() !== "terminal" &&
        !inPopup &&
        !quickOpenVisible &&
        !findVisible &&
        (e.key === "]" || e.key === "}" || e.key === "[" || e.key === "{")
      ) {
        e.preventDefault();
        const direction = e.key === "]" || e.key === "}" ? 1 : -1;
        // Tab cycling is per team for the terminal and the viewer alike.
        useViewerStore.getState().cycleTab(teamKey, getActivePaneType(), direction);
      }
      if (e.key === "Escape" && findVisible) {
        // Clear DOM-injected marks before React re-renders to prevent
        // reconciliation crash on mutated DOM nodes
        if (viewerContentRef.current) {
          const marks = viewerContentRef.current.querySelectorAll("mark[data-find]");
          marks.forEach((mark) => {
            const parent = mark.parentNode;
            if (parent) {
              parent.replaceChild(document.createTextNode(mark.textContent ?? ""), mark);
              parent.normalize();
            }
          });
        }
        setFindVisible(false);
      }
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `teamKey` is stable for the component lifetime (one mount per team). The handler's `!isScopeActive(teamKey)` early-return keeps non-active-team reads out — only the active team's keystrokes drive viewer actions. Re-registering on workspace switches within the team would churn document listeners for no behavior change.
  }, [findVisible, quickOpenVisible]);

  // Mouse back/forward button navigation
  // WebKit in Tauri does not emit PointerEvent for auxiliary buttons — use mousedown
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      // T.6 (Task 8): only the active team responds to mouse-back/forward.
      if (!isScopeActive(teamKey)) return;
      if (e.button === 3) {
        e.preventDefault();
        // T.4 Phase 5: navigate(Back|Forward) take (teamKey, repoPath).
        useViewerStore.getState().navigateBack(teamKey, navRepoPath);
      } else if (e.button === 4) {
        e.preventDefault();
        useViewerStore.getState().navigateForward(teamKey, navRepoPath);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [navRepoPath, teamKey]);

  useEffect(() => {
    const find = (event: Event) => {
      if (
        event.target instanceof Element &&
        viewerContentRef.current?.contains(event.target) &&
        event.target.closest("[data-viewer-text-menu]")
      )
        setFindVisible(true);
    };
    document.addEventListener("codesense:find", find);
    return () => document.removeEventListener("codesense:find", find);
  }, []);

  // Suppress default context menu globally in the viewer
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      const target = e.target as HTMLElement;
      if (target.closest('[data-slot="context-menu-trigger"]')) return;
      e.preventDefault();
    };
    document.addEventListener("contextmenu", handler);
    return () => document.removeEventListener("contextmenu", handler);
  }, []);

  // Own code/config files complement the already-scanned effective doc overlay.
  useEffect(() => {
    if (!quickOpenVisible || !quickOpenRoot || !isActiveView) return;
    let cancelled = false;
    setQuickOpenFiles([]);
    setQuickOpenError(null);
    setQuickOpenLoading(true);
    const deadline = setTimeout(() => {
      cancelled = true;
      setQuickOpenLoading(false);
      setQuickOpenError("Loading team files timed out. Close and reopen to retry.");
    }, CONTENT_SCAN_TIMEOUT_MS);
    const attempt = () => {
      requestContentFileIndex(quickOpenRoot)
        .then((files) => {
          if (cancelled) return;
          clearTimeout(deadline);
          setQuickOpenFiles(files);
          setQuickOpenLoading(false);
        })
        .catch((error: unknown) => {
          if (cancelled) return;
          clearTimeout(deadline);
          setQuickOpenLoading(false);
          const notIndexed = isNotIndexedError(error);
          setQuickOpenError(
            notIndexed
              ? fileListErrorMessage(error)
              : "Couldn't read team files. Close and reopen to retry.",
          );
          toastError(
            "Couldn't read team files: ",
            notIndexed ? new Error(fileListErrorMessage(error)) : error,
          );
        });
    };
    attempt();
    return () => {
      cancelled = true;
      clearTimeout(deadline);
    };
  }, [quickOpenVisible, quickOpenRoot, navRepoPath, isActiveView]);

  const handleFileSelect = useCallback(
    (
      filePath: string,
      options?: { openInNewTab?: boolean; hash?: string; keepLayout?: boolean },
    ) => {
      const store = useViewerStore.getState();
      const fileSource = resolveFileSource(filePath, resolveContentSource(navRepoPath));
      const fileRepoPath =
        fileSource.source.kind === "private" ? fileSource.contentRoot : navRepoPath;
      const fileOptions =
        options?.keepLayout || options?.hash
          ? {
              ...(options.keepLayout ? { keepLayout: true } : {}),
              ...(options.hash ? { hash: options.hash } : {}),
            }
          : undefined;
      if (options?.openInNewTab) store.openFile(teamKey, fileRepoPath, filePath, fileOptions);
      else if (fileOptions) store.openFilePreview(teamKey, fileRepoPath, filePath, fileOptions);
      else store.openFilePreview(teamKey, fileRepoPath, filePath);
    },
    [navRepoPath, teamKey],
  );

  // SLW-FX-22: ⌘P forwarded from a focused HTML iframe (which swallows the
  // keydown) reaches the SAME quick-open the parent keydown handler fires —
  // html↔markdown parity. Memoized (like handleFileSelect) so the HtmlViewer
  // message listener that depends on this prop doesn't re-subscribe every render.

  const handleContextMenuCapture = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    const target = e.target as HTMLElement;
    const external = target.closest<HTMLAnchorElement>(
      'a[href^="http://" i], a[href^="https://" i]',
    );
    if (external) {
      setContextMenuTarget({ path: external.href, external: true });
      return;
    }
    const internal = target.closest<HTMLAnchorElement>("a[data-internal-href]");
    if (internal && internal.dataset.internalHref) {
      const href = internal.getAttribute("href") ?? "";
      const fragment = href.split("#")[1];
      setContextMenuTarget({
        path: internal.dataset.internalHref,
        ...(fragment ? { hash: `#${fragment}` } : {}),
      });
      return;
    }
    setContextMenuTarget(null);
  }, []);

  const handleFileDoubleClick = useCallback(
    (filePath: string) => {
      // SW-FX-10: open-or-switch (pins a preview tab). Single-click
      // (`onFileSelect`) and meta-click (terminal) are unchanged; only
      // double-click routes here.
      useViewerStore.getState().openFile(teamKey, navRepoPath, filePath);
    },
    [navRepoPath, teamKey],
  );

  // NB-7 / AC-1: Stable PipelineStatusNav callback refs. Reading repo via
  // resolveContentSource(navRepoPath) inside the body keeps deps to [navRepoPath] only,
  // so changes to other render-scoped state (open tabs, viewer settings, …)
  // do not bust FolderHeaderRow / StoryRow / DocRow / GenericDirRow /
  // GenericFileRow memo wraps in `pipeline-status-nav.tsx`.
  // SW-FX-29 rule: nav-facing callbacks resolve the NAV's repo (`navRepoPath`),
  // not the workspace identity `repoPath`. They're equal for a normal
  // workspace; for an epic shell `repoPath` is "" (terminal rooting) while
  // `navRepoPath` resolves the team pipeline. Keying these off `repoPath` made
  // drilling, the story-file predicate, and PDF export silently no-op on epic
  // shells even though the nav itself rendered.

  const handleDrillPathChange = useCallback(
    (path: string) => {
      // SLW-NC-1: installed groups are browse-only siblings, never a drill
      // root. Their synthetic `installed://<repoSlug>/<path>` node paths sit
      // outside the context repo, so `syncDrillPath` would pin the pipeline
      // scope but still stamp a bogus `drillPath` (falsely flagging "drilled
      // in"). Ignore drills that target an installed node; real context
      // filesystem paths (which never carry the scheme) pass through unchanged.
      if (isInstalledNavPath(path)) return;
      const r = navSource.content;
      if (!r) return;
      // T.4 Phase 5: syncDrillPath takes teamKey for slice writes.
      useViewerStore.getState().syncDrillPath(teamKey, path, r.contextRepoPath, r.teamContentPath);
    },
    [navSource.content, teamKey],
  );

  // A drilled-into folder that disappears (deleted on disk or by a sync) drills
  // back out to the nearest folder that still exists.
  useStrandedDrillFallback({
    tree: selectedFullTree,
    loading: isPrivate ? privateTree.loading : fullTreeLoading,
    scopePath: pipelineScope,
    drillPath,
    onDrill: handleDrillPathChange,
  });

  const handleIsStoryFilePath = useCallback(
    (p: string) => {
      const contentPath =
        privateFile(p)?.contentRoot ?? resolveContentSource(navRepoPath)?.teamContentPath;
      if (!contentPath) return false;
      // PTS-2: scope base is the per-team content dir.
      return isStoryFilePredicate(p, contentPath);
    },
    [navRepoPath],
  );

  const handleExportPdf = useCallback(
    (filePath: string) => {
      const contentPath =
        privateFile(filePath)?.contentRoot ?? resolveContentSource(navRepoPath)?.teamContentPath;
      // Transient state (org switch, unattached repo, or non-synthetic
      // legacy repo where `teamContentPath` defaults to "" in config-store
      // until PTS-3 resolves it). Either case would pass an empty root to
      // `buildViewerUrl` and produce a malformed URL — surface explicitly.
      if (!contentPath) {
        toast.error("Couldn't export — content not ready");
        return;
      }
      void exportViewerToPdf(filePath, contentPath);
    },
    [navRepoPath],
  );

  // PTS-2: Shared content-root absolute path. Used for `isDrilledIn`,
  // `rootPath`, and downstream nav props — one memoized value avoids
  // repeated lookups and keeps the effect inside PipelineStatusNav from
  // re-firing when only render-scoped values change.
  const viewerRootAbsolute =
    (isPrivate ? navSource.content?.teamContentPath : repo?.teamContentPath) ?? "";
  // Keep the ordinary Source picker available when the first private scan fails.
  const displayedNavTree = useMemo<DerivedPipelineTree | null>(
    () =>
      navTree ??
      (isPrivate && privateTree.error
        ? {
            dirPath: "",
            dirName: "",
            absoluteDirPath: viewerRootAbsolute,
            stories: [],
            docs: [],
            subEpics: [],
            orderError: null,
            mainDoc: null,
          }
        : null),
    [navTree, isPrivate, privateTree.error, viewerRootAbsolute],
  );
  const navReadError = isPrivate
    ? privateTree.error
    : (fullTreeError ?? (teamContent.kind === "unavailable" ? teamContent.error : null));

  // SW-XS-15: what the nav overlay and its context menu need from the picked agent's
  // context. Absent on Team (everything): no overlay, no edits.
  const {
    agent: contextAgent,
    repo: contextRepo,
    view: contextView,
    membership: contextMembership,
    plan: contextPlan,
    edit: contextEdit,
  } = agentContext;
  const navAgentContext = useMemo<NavAgentContext | undefined>(
    () =>
      contextAgent && contextMembership
        ? {
            agentName: contextAgent.name,
            repoName: contextRepo?.repo ?? null,
            relOf: navRelResolver(viewerRootAbsolute, contextView),
            editRelOf: teamRelResolver(viewerRootAbsolute),
            defaulted: contextView?.context === null,
            membership: contextMembership,
            plan: contextPlan,
            edit: contextEdit,
          }
        : undefined,
    [
      contextAgent,
      contextRepo,
      contextView,
      contextMembership,
      contextPlan,
      contextEdit,
      viewerRootAbsolute,
    ],
  );

  // Mermaid theme re-apply — a single subscription at the viewer root
  // re-runs `mermaid.initialize` when the app theme flips. Lives here
  // instead of inside `MarkdownViewer` so the 10 kept-alive viewers
  // don't fire 10 identical init calls on every theme change.
  // `mermaid.initialize` merges config; re-passing `suppressErrors` +
  // `startOnLoad` each time keeps those flags explicit in every call.
  const { resolvedTheme } = useTheme();
  useEffect(() => {
    mermaid.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      // @ts-expect-error suppressErrors exists at runtime but is missing from type defs
      suppressErrors: true,
      theme: resolvedTheme === "dark" ? "dark" : "default",
    });
  }, [resolvedTheme]);

  // Keep-alive: the last N viewed files stay mounted (hidden via
  // display:none) so tab switches don't re-parse / re-highlight /
  // re-render mermaid. LRU touch fires on every activeTab change
  // (the tab-switch debounce was removed in 32.6 once block-chunked
  // render + rAF-batched island mounts made each commit cheap).
  const openedViewerPaths = useViewerStore((s) => s.openedViewerPaths[teamKey]) ?? EMPTY_PATHS;
  // SW-VE-3: the team's open tab list (reactive) — the edit-session prune effect
  // below reaps any editing session whose tab has closed, so an inset is never
  // orphaned (mounted but with no tab to return to).
  const openTabsForTeam = useViewerStore((s) => s.openTabs[teamKey]);
  // A tab on its Changes renders the diff whatever the file's type, so the "no preview" gate
  // below must not fire for it.
  const showsDiff = useCallback(
    (p: string) =>
      resolveFileSource(p).source.kind === navSource.source.kind &&
      isShowingDiff(tabDiff?.[p]) &&
      !!orgRoot,
    [tabDiff, orgRoot, navSource.source.kind],
  );

  // SW-SN-10: relations for the open doc's "Related" chip — team supporting_docs
  // (own-doc-keyed from the warm tree) + the context SN-9's resolveCsLink uses for
  // the private note-backlink scan. The react-flow conversion is derived from the
  // tree ONLY (not the active tab), so switching tabs doesn't re-run it; the cheap
  // own-doc lookup recomputes per doc. Null when the active tab isn't a markdown/
  // HTML doc (or in sync-diff view). The async note scan is lazy, in the chip.
  const relatedFlowNodes = useMemo(
    () => (fullPipelineTree ? derivedTreeToReactFlow(fullPipelineTree).nodes : null),
    [fullPipelineTree],
  );
  const relatedDoc = useMemo(() => {
    if (!relatedFlowNodes || !shouldShowRelatedChip(activeTabPath, activeDiff)) return null;
    return computeDocRelations(relatedFlowNodes, activeTabPath, repo?.teamContentPath ?? "");
  }, [relatedFlowNodes, activeTabPath, activeDiff, repo?.teamContentPath]);

  // SLW-MD-1: the in-file metadata band. Open state persists across file
  // switches (AC-15) and is per-team so switching teams doesn't leak it.
  const [metadataOpenByTeam, setMetadataOpenByTeam] = useState<Record<string, boolean>>({});
  const metadataOpen = metadataOpenByTeam[teamKey] ?? false;
  const metadataChipRef = useRef<HTMLButtonElement>(null);
  const toggleMetadata = useCallback(() => {
    setMetadataOpenByTeam((m) => ({ ...m, [teamKey]: !(m[teamKey] ?? false) }));
  }, [teamKey]);
  const closeMetadata = useCallback(() => {
    setMetadataOpenByTeam((m) => ({ ...m, [teamKey]: false }));
    metadataChipRef.current?.focus();
  }, [teamKey]);

  // Metadata validity for the open doc → chip dot, mirrored from the nav scan.
  const metadataValidity = useMemo(
    () => findMetadataValidity(navTree, activeTabPath),
    [navTree, activeTabPath],
  );

  // AC-13: read-only when the open path is an installed source-repo layer doc.
  const metadataReadOnly = useMemo(
    () => isReadOnlyLayeredDoc(navTree, activeTabPath),
    [navTree, activeTabPath],
  );

  // SLW-FX-21 cap 1/3: the write affordances for the Related chip. Undefined for
  // a read-only layered source doc (AC-8) → the chip stays read-only. Candidates
  // come from the in-memory tree (no disk scan) and exclude the open doc + docs
  // already related. Relate/unlink route through the ONE shared orchestration.
  const relateDocActions = useMemo<RelateDocActions | undefined>(() => {
    if (metadataReadOnly || !activeTabPath) return undefined;
    const exclude = new Set<string>([activeTabPath]);
    for (const d of relatedDoc?.supportingDocs ?? []) exclude.add(d.filePath);
    return {
      candidates: collectRelateCandidates(fullPipelineTree, exclude),
      relate: (docPath: string) => void relateDoc(activeTabPath, docPath),
      unlink: (docPath: string) => void unrelateDoc(activeTabPath, docPath),
    };
  }, [metadataReadOnly, activeTabPath, relatedDoc?.supportingDocs, fullPipelineTree]);

  // SLW-FX-21 cap 2: drag a nav file onto another → relate the dragged file INTO
  // the drop target. The nav's own predicate already blocks folders, installed
  // (browse-only) targets, self-drops, and cross-root drags.
  const handleRelateDrop = useCallback((targetFilePath: string, draggedFilePath: string) => {
    void relateDoc(targetFilePath, draggedFilePath);
  }, []);

  // The org owns metadata schemas and source repos, regardless of the nav's file source.
  const metadataOrgRoot = cloneOrgRoot ?? "";
  const { teamName: metadataTeamName, orgSlug: metadataOrgSlug } = useMemo(
    () => parseTeamKey(teamKey),
    [teamKey],
  );

  // SW-VE-5 (AC-3): the active tab's scope, joined from the resident source-repo
  // scans and then the nav tree by path (pure tree lookup — no new command, no
  // blocking I/O). SW-XS-20: a file inherited from a parent team stays editable —
  // its path IS the parent's file, so every write lands there — and the toolbar
  // says so. `null` for an own file. The reference team is THIS view's team.
  const scopeOfPath = useCallback(
    (path: string): FileScope | null =>
      activeFileScope(installedScans, navTree ?? null, path, metadataTeamName),
    [installedScans, navTree, metadataTeamName],
  );
  const activeScope = useMemo(
    () => (activeTabPath ? scopeOfPath(activeTabPath) : null),
    [scopeOfPath, activeTabPath],
  );

  // SW-XS-20: "Copy into ‹team›" from the viewer — the open inherited file is
  // copied to the same path in this team's content, which then wins for the
  // team; the team's copy opens. Refused when the team already has that path.
  const copyActiveIntoTeam = useCallback(async () => {
    const team = activeScope?.isInherited ? activeScope.sourceTeam : null;
    if (!activeTabPath || !team || !metadataTeamName || !repo?.teamContentPath) return;
    const r = await copyIntoTeam(activeTabPath, team, repo.teamContentPath, metadataTeamName);
    if (r.ok) {
      toast.success(`Copied into ${metadataTeamName}`);
      useFileRefreshStore.getState().bumpFileRefresh(navRepoPath);
      useViewerStore.getState().openFile(teamKey, navRepoPath, r.path);
    } else {
      toast.error(r.error);
    }
  }, [activeScope, activeTabPath, metadataTeamName, repo, navRepoPath, teamKey]);

  // Configured source repos for the active team (attached sorted first) →
  // the source_repo constrained picker + the `team` select's known teams.
  const activeOrg = useMemo(
    () => orgs.find((o) => o.slug === metadataOrgSlug) ?? null,
    [orgs, metadataOrgSlug],
  );
  const knownTeams = useMemo(() => (activeOrg?.teams ?? []).map((t) => t.name), [activeOrg]);
  const [sourceRepoOptions, setSourceRepoOptions] = useState<SourceRepoOption[]>([]);
  useEffect(() => {
    if (!metadataOpen || !metadataOrgRoot || !metadataOrgSlug || !metadataTeamName) {
      return;
    }
    let cancelled = false;
    void listSourceReposForTeam(metadataOrgRoot, metadataOrgSlug, metadataTeamName)
      .then((repos) => {
        if (!cancelled) setSourceRepoOptions(repos);
      })
      .catch(() => {
        if (!cancelled) setSourceRepoOptions([]);
      });
    return () => {
      cancelled = true;
    };
  }, [metadataOpen, metadataOrgRoot, metadataOrgSlug, metadataTeamName]);

  // The Metadata chip shows on any open, non-sync-view DOC. Suppressed in
  // sync/redline view (matches Related/Versions chips).
  // SW-VE-2 (AC-7): gate on `isFrontmatterEditable` (markdown/html only), NOT
  // `isViewerFilePath`. VE-1 used `isViewerFilePath` because back then that set
  // WAS {markdown, html, image, excalidraw}; VE-2 widens it with `code` types,
  // so `isViewerFilePath(".yaml")` is now true. Gating the metadata editor on it
  // would re-expose the frontmatter band for config/code files and
  // `apply_metadata_edit` (fail-open) would inject a `---` block and corrupt
  // them — the exact Major VE-1's review closed. Frontmatter is a doc concept;
  // this predicate keeps the editor doc-only, decoupled from the wider
  // previewable set.
  // SW-XS-20: a file inherited from a parent team keeps the Metadata chip and
  // Edit — its writes land in the parent's file — and the toolbar adds the
  // explainer saying so (`inheritedActive`).
  // SW-VE-8: Edit covers ALL writable text (markdown / html / code /
  // unknown-text) via `isEditableTextFile` — text-ness AND previewable (so a
  // known-binary catch-all like `archive.zip` stays excluded, AC-6) — never in
  // a diff (VE-2 R-5) or on a virtual management tab (SW-OC-4). SW-XS-20: a file
  // from a parent team keeps Edit; its edits save to that team's file.
  const { inheritedActive, showMetadataChip, canEditActive } = docToolbarGates(
    activeTabPath,
    activeScope,
    !!activeDiff,
  );
  // SW-VE-2 (AC-4) + SW-VE-8: the read-only / edit chip renders exactly when the
  // active file is editable text — i.e. `canEditActive` — as the
  // "Read-only · Edit" switch (or "Editing"). Read-only IS still the visible
  // default, but for an editable file it's an actionable switch.
  const showReadOnlyChip = canEditActive;

  // SW-VE-3: deliberate edit mode. `editingPaths` are the files the user has
  // explicitly opened for editing (read-only is the default). Concurrent per-tab
  // sessions: each editing file keeps its own inset, which stays mounted while
  // the file is in `renderList`, so switching tabs never drops a buffer.
  const [editingPaths, setEditingPaths] = useState<string[]>([]);
  const [editDirtyByPath, setEditDirtyByPath] = useState<Record<string, boolean>>({});
  const editingActive = !!activeTabPath && editingPaths.includes(activeTabPath);
  const activeEditDirty = !!activeTabPath && !!editDirtyByPath[activeTabPath];
  // Per-editing-file scope for the "Editing in <scope>" chip — memoized so the
  // tree walk doesn't run on every view re-render (only when the set of edited
  // files or the scans change).
  const editingScopeByPath = useMemo(() => {
    const m: Record<string, FileScope | null> = {};
    for (const p of editingPaths) {
      m[p] = scopeOfPath(p);
    }
    return m;
  }, [editingPaths, scopeOfPath]);
  const enterEdit = useCallback(() => {
    if (!activeTabPath) return;
    setEditingPaths((prev) => (prev.includes(activeTabPath) ? prev : [...prev, activeTabPath]));
  }, [activeTabPath]);
  const exitEdit = useCallback(
    (filePath: string) => {
      setEditingPaths((prev) => prev.filter((p) => p !== filePath));
      setEditDirtyByPath((prev) => {
        if (!(filePath in prev)) return prev;
        const { [filePath]: _drop, ...rest } = prev;
        return rest;
      });
      // SW-FX-139: exit (Done, or a discard that leaves the disk copy) re-reads this
      // path in the read view so it is never stale. Sibling files stay cached.
      markContentPathChanged(navRepoPath, filePath);
    },
    [navRepoPath],
  );
  const setEditDirtyForPath = useCallback((filePath: string, dirty: boolean) => {
    setEditDirtyByPath((prev) =>
      prev[filePath] === dirty ? prev : { ...prev, [filePath]: dirty },
    );
  }, []);
  // SW-VE-3: reap an edit session whose tab has closed so its inset is never
  // orphaned (mounted-but-unreachable). A DIRTY edit can't be closed (the store
  // guards closeTab / closeOtherTabs / closeAllTabs), so this only ever removes
  // a CLEAN session — the buffer had nothing to lose.
  useEffect(() => {
    const open = new Set((openTabsForTeam ?? []).map((t) => t.path));
    setEditingPaths((prev) => {
      const next = prev.filter((p) => open.has(p));
      return next.length === prev.length ? prev : next;
    });
    setEditDirtyByPath((prev) => {
      const keys = Object.keys(prev);
      if (keys.every((p) => open.has(p))) return prev;
      const next: Record<string, boolean> = {};
      for (const p of keys) if (open.has(p)) next[p] = prev[p];
      return next;
    });
  }, [openTabsForTeam]);
  // Mirror the dirty-edit paths into the store so `closeTab` can't silently drop
  // an unsaved buffer (it re-activates the tab instead).
  useEffect(() => {
    const dirty = editingPaths.filter((p) => editDirtyByPath[p]);
    useViewerStore.getState().setEditingDirtyPaths(teamKey, dirty);
  }, [teamKey, editingPaths, editDirtyByPath]);
  // Clear the dirty-edit guard for this team on unmount (team close).
  useEffect(
    () => () => {
      useViewerStore.getState().setEditingDirtyPaths(teamKey, []);
    },
    [teamKey],
  );
  // Latest edit-entry capability for the ⌘E keydown branch (avoids stale closure).
  const editEntryRef = useRef<{ canEnter: boolean; enter: () => void }>({
    canEnter: false,
    enter: () => {},
  });
  useEffect(() => {
    editEntryRef.current = { canEnter: canEditActive && !editingActive, enter: enterEdit };
  }, [canEditActive, editingActive, enterEdit]);
  // SW-VE-8: honor a `viewer.edit-file` shortcut / File-menu request routed
  // through the store (the registry `run()` can't reach `editEntryRef`). React
  // only to THIS team's request, once per nonce, through the SAME gate the ⌘E
  // keydown used (no-op on read-only / non-editable / already
  // editing). The ref is seeded with the current nonce so a pre-existing
  // request never auto-enters edit on mount.
  const editFileRequest = useViewerStore((s) => s.editFileRequest);
  const handledEditRequestNonceRef = useRef(editFileRequest?.nonce ?? 0);
  useEffect(() => {
    // Skipping the ref update on a teamKey mismatch is safe ONLY because
    // `requestEditActiveFile` bumps a single GLOBAL-MONOTONIC nonce — a team
    // never sees a nonce it already handled reappear. If that ever becomes
    // per-team, record the last-seen nonce regardless of team match.
    if (!editFileRequest || editFileRequest.teamKey !== teamKey) return;
    if (editFileRequest.nonce === handledEditRequestNonceRef.current) return;
    handledEditRequestNonceRef.current = editFileRequest.nonce;
    const entry = editEntryRef.current;
    if (entry.canEnter) entry.enter();
  }, [editFileRequest, teamKey]);
  useLayoutEffect(() => {
    if (activeTabPath) {
      useViewerStore.getState().touchViewerPath(teamKey, activeTabPath);
    }
  }, [activeTabPath, teamKey]);

  // Background preload: warm `fileContentCache` for persisted `openTabs`
  // on first activation of a repo, so every restored tab's first click
  // hits the cache on `readFile` and only pays the worker-parse cost
  // (~100ms async, non-blocking). Paced through `requestIdleCallback`
  // with one `readFile` in flight at a time.
  //
  // Earlier (32.3) this called `touchViewerPath`, which added paths to
  // `openedViewerPaths` and mounted a hidden MarkdownViewer per tab.
  // Each mount fired readFile + worker-parse in React's commit phase,
  // and with the 32.4 worker the main thread no longer "feels busy"
  // during parse, so `requestIdleCallback` stopped yielding to user
  // input reliably — preload mounts would cascade into the same React
  // commit as a Cmd+Shift+] keystroke and block the tab-switch paint
  // for 300ms+. Cache-only preload eliminates that collision: no new
  // MarkdownViewer mounts, no React cascade, and the worker stays free
  // for the user's actual click.
  const hasPreloadedViewers = useRef(false);
  useEffect(() => {
    if (!isActiveView || hasPreloadedViewers.current) return;
    hasPreloadedViewers.current = true;

    const tabs = useViewerStore.getState().openTabs[teamKey] ?? [];
    const active = useViewerStore.getState().activeTab[teamKey] ?? "";
    const queue = tabs
      .map((t) => t.path)
      // Warm the byte cache only for named viewer files. SW-VE-2: code/config
      // files (yaml/toml/json/…) are now viewer files, so they DO preload — their
      // first open hits the cache and CodeFileViewer skips the IPC read. True
      // binaries and unknown catch-all text (not in VIEWER_EXTENSIONS) stay out
      // of preload: `isViewerFilePath` is false for them, so no wasted IPC.
      .filter((p) => p && p !== active && isViewerFilePath(p) && !fileContentCache.get(p))
      .slice(0, VIEWER_PERF.keepAliveMaxViewers - 1);
    if (queue.length === 0) return;

    let cancelled = false;
    let handle: number | null = null;
    const scheduleIdle =
      typeof window.requestIdleCallback === "function"
        ? (cb: () => void) => window.requestIdleCallback(cb, { timeout: 2000 })
        : (cb: () => void) => window.setTimeout(cb, 100);
    const cancelIdle =
      typeof window.cancelIdleCallback === "function"
        ? (id: number) => window.cancelIdleCallback(id)
        : (id: number) => window.clearTimeout(id);

    const step = (i: number) => {
      if (cancelled || i >= queue.length) return;
      handle = scheduleIdle(() => {
        if (cancelled) return;
        const path = queue[i];
        // SR-9: stamp the prefetch cache entry with the SAME key the viewer's
        // Layer-1 fast-path compares against (`contentRefreshKeys`, not the scan
        // counter) — else the tokens diverge on a counter-only bump and the cache
        // never hits, turning the prefetch into extra reads.
        const refreshPath = privateFile(path)?.contentRoot ?? navRepoPath;
        const refreshKey = useFileRefreshStore.getState().contentRefreshKeys[refreshPath] ?? 0;
        readFile(path)
          .then((text) => {
            if (cancelled) return;
            fileContentCache.set(path, text, refreshKey);
            step(i + 1);
          })
          .catch(() => {
            // Preload failure is non-fatal — user click will re-attempt
            // readFile via the MarkdownViewer's own effect 1.
            if (!cancelled) step(i + 1);
          });
      });
    };
    step(0);

    return () => {
      cancelled = true;
      if (handle !== null) cancelIdle(handle);
    };
  }, [isActiveView, navRepoPath, teamKey]);

  // Synchronization guard: inject the current active path if the
  // store LRU hasn't caught up yet, so there's always a viewer for
  // the content we intend to display. Cap to prevent accidental
  // overflow.
  const renderList = useMemo(() => {
    // SW-VE-3: files being edited (+ the active file) are mounted FIRST so they
    // survive the keep-alive `slice` cap — a live edit buffer must never be
    // evicted mid-session. Editing paths are pruned to open tabs (above), so
    // prioritizing them never mounts an unreachable/orphaned inset.
    const priority: string[] = [];
    for (const p of editingPaths) if (!priority.includes(p)) priority.push(p);
    if (activeTabPath && !priority.includes(activeTabPath)) priority.push(activeTabPath);
    const rest = openedViewerPaths.filter(
      (p) =>
        !priority.includes(p) &&
        (!/\.html?$/i.test(p) || openTabsForTeam?.some((tab) => tab.path === p)),
    );
    return [...priority, ...rest].slice(0, VIEWER_PERF.keepAliveMaxViewers);
  }, [openedViewerPaths, activeTabPath, editingPaths, openTabsForTeam]);

  const retiringRenderList = useRetiringFrames(
    renderList,
    (p) => p,
    (f, p) => f.documentPath === p && f.slotId === "centre" && f.teamKey === teamKey,
  );

  // SV-ENG-120: a tab switch names what is mounted (render list plus frames kept for their
  // retire flush), so a document that survives and is reminted is not reported as unmounted.
  const timingPrevTab = useRef<{ active: string | undefined; mounted: string[] } | null>(null);
  useEffect(() => {
    const prev = timingPrevTab.current;
    const mounted = retiringRenderList.filter((p) => /\.html?$/i.test(p));
    timingPrevTab.current = { active: activeTabPath, mounted };
    if (!prev || prev.active === activeTabPath) return;
    const added = mounted.filter((p) => !prev.mounted.includes(p));
    const unmounted = prev.mounted.filter((p) => !mounted.includes(p));
    if (!added.length && !unmounted.length && !/\.html?$/i.test(activeTabPath ?? "")) return;
    bridgeTiming(
      () =>
        `tab-switch t=${nowMs()} team=${teamKey} from=${prev.active} to=${activeTabPath} documentVisibility=${document.visibilityState} targetWasMounted=${!!activeTabPath && prev.mounted.includes(activeTabPath)} mountedNow=${added.join("|") || "-"} unmounted=${unmounted.join("|") || "-"} retiring=${retiringRenderList.filter((p) => !renderList.includes(p)).join("|") || "-"} keepAliveCap=${VIEWER_PERF.keepAliveMaxViewers}`,
    );
  }, [retiringRenderList, renderList, activeTabPath, teamKey]);

  const view = (
    // The team's nav and viewer. The workbench sizes this region against the CLI's split and
    // hides it while the CLI is full screen; the CLI, the tray and the right pane live there too.
    <div
      data-team-view={teamKey}
      data-surface="viewer"
      className="flex h-full overflow-hidden bg-background text-foreground"
    >
      {/* Left nav sidebar — status nav for pipeline repos, file tree fallback */}
      <div
        ref={sidebarRef}
        data-surface="sidebar"
        className="h-full flex flex-col flex-shrink-0"
        // SW-FX-8 a11y: when collapsed (width 0, but kept mounted), mark
        // the subtree inert so its focusable tree items/buttons leave the
        // tab order + accessibility tree and focus can't be stranded
        // inside the hidden nav. `overflow:hidden` alone only clips visually.
        inert={navCollapsed}
        style={{
          // SW-FX-8: collapse to 0 width (instant, no transition) WITHOUT
          // overwriting the user's committed sidebarWidth; keep mounted
          // (overflow hidden) so PipelineStatusNav ephemeral state survives.
          width: navCollapsed ? 0 : sidebarWidth,
          overflow: "hidden",
          fontSize: `${treeFontPx}px`,
          backgroundColor: "var(--cs-sidebar-bg, color-mix(in srgb, var(--card) 50%, transparent))",
          color: "var(--cs-sidebar-text, var(--foreground))",
        }}
      >
        {/* The scope row (context pivot + the context repo's status button). The
                    org/team identity lives only in the title bar. Mounted for the active
                    team only, so backgrounded teams don't each read their context repo. */}
        {isActiveView ? (
          <LeftNavHeader
            teamKey={teamKey}
            navRepoPath={navRepoPath}
            resolvedSource={navSource}
            pivot={agentContext}
            agentContext={isPrivate ? undefined : navAgentContext}
          />
        ) : null}
        {navSource.note && (
          <p role="status" className="px-2 text-xs text-muted-foreground">
            {navSource.note}
          </p>
        )}
        {isPrivate && navSource.privateSource?.baselineStarted && (
          <p role="status" className="px-2 text-xs text-muted-foreground">
            Started tracking your private files. Edits from now on show here.
          </p>
        )}
        {isPrivate && navSource.privateSource?.baselineError && (
          <p role="status" className="px-2 text-xs">
            Couldn't set a starting point ({navSource.privateSource.baselineError}), so every
            private file shows as new. <button onClick={navSource.retryBaseline}>Try again</button>
          </p>
        )}
        {navReadError && (
          <div role="alert" className="px-3 py-2 text-sm text-muted-foreground">
            <p>{navReadError}</p>
            {!isPrivate && teamContent.kind === "unavailable" ? (
              <button type="button" onClick={() => useUiStore.getState().setTeamSwitcherOpen(true)}>
                Choose a team
              </button>
            ) : (
              <button type="button" onClick={isPrivate ? privateTree.refetch : refetchTree}>
                Retry
              </button>
            )}
          </div>
        )}
        {(isPrivate ? privateTree.loading : fullTreeLoading || teamContent.kind === "loading") &&
          !navTree && (
            <div role="status" className="px-3 py-2 text-sm text-muted-foreground">
              Loading team context…
            </div>
          )}
        <FileNavSlotMount teamKey={teamKey} selected={activeTabPath}>
          {
            displayedNavTree ? (
              <PipelineStatusNav
                teamKey={teamKey}
                scopeTree={fullPipelineTree ?? undefined}
                fileProjectionTree={fileProjectionTree ?? undefined}
                tree={displayedNavTree}
                // SF-8 review iter-1 (EC-2): the relaxed skeleton gate
                // can land on this branch with `pipelineGraph === null`
                // (loading=false, tree=null case). `navTree` being
                // truthy means `scopedPipelineTree` (→ `fullPipelineTree`)
                // is truthy → the memo's `pipelineGraph` is non-null in
                // practice; coerce null to undefined to satisfy the
                // optional prop type. (SLW-NC-1: `navTree` = context
                // scoped tree + any installed groups.)
                graph={
                  (isPrivate && privateTree.tree
                    ? buildPipelineGraph(privateTree.tree)
                    : pipelineGraph) ?? undefined
                }
                repoPath={navRepoPath}
                selectedPath={activeTabPath}
                onFileSelect={handleFileSelect}
                onFileDoubleClick={handleFileDoubleClick}
                onDrillPathChange={handleDrillPathChange}
                onStatusChange={handleNavStatusChange}
                repoName={repo?.repoName}
                rootDisplayName={isPrivate ? "Private" : rootDisplayName}
                isDrilledIn={!!drillPath && drillPath !== viewerRootAbsolute}
                rootPath={viewerRootAbsolute}
                onOpenStoryWorkspace={openStoryWorkspaceByPath}
                renderOpenCli={renderOpenCli}
                onOpenEpicTerminal={handleOpenEpicTerminal}
                isStoryFilePath={handleIsStoryFilePath}
                onExportPdf={handleExportPdf}
                viewerRootPath={viewerRootAbsolute}
                refreshKey={navRefreshKey}
                // SLW-NC-1 Phase 2: provenance badge inputs for installed
                // group headers (P2-AC-3). Memoized/stable — no blocking.
                provenance={navProvenance}
                onRelateDrop={handleRelateDrop}
                sprintRegistry={navSprintRegistry}
                privateFiles={isPrivate ? privateFiles : undefined}
                moveActionSlot={
                  isPrivate
                    ? (path) => {
                        const file = privateFiles.get(path);
                        return file ? <ScratchTreeBadge file={file} /> : null;
                      }
                    : moveActionSlot
                }
                agentContext={isPrivate ? undefined : navAgentContext}
              />
            ) : null
            /* SF-8: when `fullPipelineTree` is non-null but the scoped
                 subtree is empty (a drilled scope that resolves to no
                 entries), render nothing here — the user already passed
                 the SF-8 skeleton gate, so a layout-shift to a "Loading"
                 placeholder would be misleading. */
          }
        </FileNavSlotMount>
      </div>

      {/* SW-FX-8: nav resize + collapse affordance — the border IS the
                control. Expanded = drag to resize / click to collapse;
                collapsed = the kept border is the click-to-expand target. */}
      <PaneCollapseDivider
        edge="left"
        collapsed={navCollapsed}
        onToggle={toggleNav}
        hotkeyActionId="viewer.toggle-nav"
        paneLabel="sidebar"
        resize={navResize}
        size={navSize}
        borderColor="var(--cs-sidebar-border, var(--border))"
        data-testid="nav-collapse-divider"
      />

      {/* Markdown viewer */}
      <div
        className="flex-1 flex flex-col overflow-hidden"
        style={{ backgroundColor: "var(--cs-viewer-bg, transparent)" }}
        data-viewer-content
        tabIndex={-1}
      >
        {/* SW-VE-16: the file-viewer top toolbar is ONE band hosting both zones on
                    one baseline (AC-7). `FileViewerToolbar` owns the shared chrome; this host
                    just computes which zones have content. SW-OC-4 (folded from SW-OC-1): a
                    virtual management tab is a synthetic surface — suppress both zones. */}
        <FileViewerToolbar
          hasChips={
            !isVirtualTabPath(activeTabPath) &&
            (!!relatedDoc || showMetadataChip || showReadOnlyChip || inheritedActive)
          }
          hasDiff={
            activeSourceMatches &&
            !isVirtualTabPath(activeTabPath) &&
            !!activeTabPath &&
            !!orgRoot &&
            (!!tabDiff?.[activeTabPath] || changeByPath.has(activeTabPath))
          }
          teamKey={teamKey}
          activeTabPath={activeTabPath}
          change={changeByPath.get(activeTabPath)}
          diff={tabDiff?.[activeTabPath] ?? null}
          relatedDoc={relatedDoc}
          relateDocActions={relateDocActions}
          inheritedActive={inheritedActive}
          activeScope={activeScope}
          copyIntoTeamName={metadataTeamName}
          onCopyIntoTeam={() => void copyActiveIntoTeam()}
          showMetadataChip={showMetadataChip && !editingActive}
          showReadOnlyChip={showReadOnlyChip}
          canEditActive={canEditActive}
          editingActive={editingActive}
          editDirty={activeEditDirty}
          onEditActive={enterEdit}
          metadataOpen={metadataOpen}
          toggleMetadata={toggleMetadata}
          metadataValid={metadataValidity?.metadataValid}
          metadataChipRef={metadataChipRef}
          regionId="metadata-band-region"
          onOpenFile={(filePath) =>
            useViewerStore.getState().openFile(teamKey, navRepoPath, filePath)
          }
        />

        {/* SLW-MD-1: the in-file metadata band mounts ABOVE the doc.
                    As a normal-flow sibling inside the flex-column
                    `data-viewer-content`, it pushes the flex-1 doc content down
                    without touching the doc's own scroll container (the per-tab
                    `h-full` div below), so there's no scroll jump. */}
        {/* SW-VE-8 (AC-3): raw-source edit is canonical — while a file
                    is being edited, suppress its editable frontmatter band so it
                    never co-presents two frontmatter writers. The Metadata chip
                    is likewise hidden while editing (gated above); it stays a
                    read-mode convenience otherwise. */}
        {metadataOpen && showMetadataChip && !editingActive && (
          <MetadataBand
            key={`metadata-band-${activeTabPath}`}
            filePath={activeTabPath}
            repoPath={navRepoPath}
            orgRoot={metadataOrgRoot || undefined}
            orgSlug={metadataOrgSlug || undefined}
            team={metadataTeamName || undefined}
            sourceRepoOptions={sourceRepoOptions}
            knownTeams={knownTeams}
            readOnly={metadataReadOnly}
            onClose={closeMetadata}
            regionId="metadata-band-region"
          />
        )}
        <div style={{ display: "contents" }}>
          <FindBar
            visible={findVisible}
            onClose={() => setFindVisible(false)}
            containerRef={viewerContentRef}
            containerKey={findContainerKey}
            contentVersion={viewerContentVersion}
          />
          <ContextMenu>
            <ContextMenuTrigger asChild>
              <div
                className="flex-1 overflow-hidden relative"
                data-viewer-text-menu
                onContextMenuCapture={handleContextMenuCapture}
              >
                {/* SW-VE-1/2: a listed file previews unless it is a known
                            binary (archive/exe/font/media) — text/code/config all
                            render (code via CodeFileViewer, which is the final
                            text-vs-binary authority via a content sniff); only true
                            binaries fall to the "no preview" placeholder. Listing is
                            not rendering. Sync redline keeps its own path (the
                            `redline` directive renders the diff regardless). */}
                {retiringRenderList.map((p) => {
                  const privatePath = privateFile(p);
                  const fileSource = resolveFileSource(p, repo, cloneOrgRoot);
                  const refreshPath = privatePath?.contentRoot ?? navRepoPath;
                  const contentRefreshKey = contentRefreshKeys[refreshPath] ?? 0;
                  const lastChangedContextFile = lastChangedContextFiles[refreshPath] ?? null;
                  const diffOn = showsDiff(p);
                  const previewable = isPreviewableFilePath(p) || diffOn;
                  const readOnlyContent = previewable ? (
                    renderViewer(
                      {
                        filePath: p,
                        hostTeamKey: teamKey,
                        refreshKey: contentRefreshKey,
                        teamContentPath: fileSource.contentRoot,
                        onNavigate: handleFileSelect,
                      },
                      {
                        changedPaths: lastChangedContextFile,
                        pendingHash: pendingHash?.path === p ? pendingHash.hash : null,
                        onPendingHashConsumed: handlePendingHashConsumed,
                        onContentCommitted: bumpViewerContentVersion,
                        // RF-perf-12: preserve this file's markdown scroll
                        // across a team-unmount / eviction remount.
                        viewStateKey: fileViewKey(teamKey, p),
                        // SW-DX-5: the already-scanned tree an inline
                        // `csquery` block first-paints against (reused,
                        // no second scan — the island stays live via
                        // onStatus, not via this prop changing).
                        tree: fullPipelineTree,
                        // A tab on its Changes renders the file against
                        // GitHub (once org_root resolves). A renamed doc
                        // diffs by its old baseline path.
                        redline:
                          diffOn && orgRoot
                            ? redlineFor(tabDiff?.[p], {
                                orgRoot: fileSource.orgRoot ?? orgRoot,
                                teamKey,
                                navRepoPath: privatePath?.contentRoot ?? navRepoPath,
                                active: p === activeTabPath,
                              })
                            : null,
                      },
                    )
                  ) : (
                    <FileWarningPlaceholder
                      message="No preview available"
                      filename={p.split("/").pop() ?? p}
                      testId="nav-no-preview"
                    />
                  );
                  return (
                    <div
                      key={p}
                      ref={p === activeTabPath ? viewerContentRef : undefined}
                      className="h-full"
                      style={{ display: p === activeTabPath ? undefined : "none" }}
                    >
                      {privatePath && (
                        <span
                          className={`inline-flex items-center gap-1 rounded bg-muted px-1.5 text-[0.65rem] text-muted-foreground ${privateFiles.get(p)?.replacesTeam ? "ring-1 ring-inset ring-border" : ""}`}
                        >
                          <Lock size={10} aria-hidden />
                          {privateFiles.get(p)?.replacesTeam
                            ? "Private · replaces team's"
                            : "Private"}
                        </span>
                      )}
                      {editingPaths.includes(p) ? (
                        // SW-VE-3: deliberate edit mode replaces the
                        // read-only viewer for this file. Kept OUT of the
                        // shared `renderViewer` (read-only-pure so a review
                        // surface never gains an editable buffer).
                        // On its Changes the tab must stay read-only, so we
                        // render the diff and keep the editor MOUNTED+HIDDEN
                        // (the buffer survives the round-trip, never
                        // dropped). `active={!diffOn}` makes the hidden
                        // editor readOnly so the `display:none` blur can't
                        // fire a blur-save — no write is reachable there.
                        <>
                          <div style={{ display: diffOn ? "none" : "contents" }}>
                            <ContextEditorInset
                              filePath={p}
                              scope={editingScopeByPath[p] ?? null}
                              refreshKey={contentRefreshKey}
                              changedPaths={lastChangedContextFile}
                              onExit={() => exitEdit(p)}
                              onDirtyChange={(d) => setEditDirtyForPath(p, d)}
                              active={!diffOn}
                              // RF-perf-12: same key as the read-only
                              // viewer for this file (they're mutually
                              // exclusive in pipeline view) — cursor +
                              // scroll survive an unmount→remount.
                              viewStateKey={fileViewKey(teamKey, p)}
                            />
                          </div>
                          {diffOn && readOnlyContent}
                        </>
                      ) : (
                        // SW-CP-29: the active read-only tab gains an
                        // "Authored | Merged preview" toggle for a
                        // merge-marked config file (self-hiding otherwise).
                        // Only the active tab is wrapped so hidden tabs
                        // don't each load merge marks.
                        <MergeMarkedViewer
                          absPath={p}
                          teamContentPath={repo?.teamContentPath ?? ""}
                          enabled={!diffOn && p === activeTabPath}
                        >
                          {readOnlyContent}
                        </MergeMarkedViewer>
                      )}
                    </div>
                  );
                })}
                {!activeTabPath && (
                  <div className="absolute inset-0 flex items-center justify-center px-8 text-center text-sm text-muted-foreground">
                    {fullTreeLoading || teamContent.kind === "loading"
                      ? "Loading team context…"
                      : fullTreeError || teamContent.kind === "unavailable"
                        ? "Your context is unavailable. See the sidebar for recovery."
                        : "Open a file from the sidebar, or create a document to build your team context."}
                  </div>
                )}
                {/* SW-FX-62: the SW-VE-5 read-only explainer + the Related /
                            Versions / Metadata / SW-VE-2 Read-only chips no longer float
                            here — they moved into the docked FileViewerToolbar band
                            above the content region so nothing overlays the doc. */}
              </div>
            </ContextMenuTrigger>
            <ContextMenuContent>
              {contextMenuTarget ? (
                <>
                  <ContextMenuItem
                    onSelect={() => {
                      if (!contextMenuTarget) return;
                      if (contextMenuTarget.external) {
                        void openUrl(contextMenuTarget.path);
                        return;
                      }
                      handleFileSelect(contextMenuTarget.path, {
                        openInNewTab: true,
                        ...(contextMenuTarget.hash ? { hash: contextMenuTarget.hash } : {}),
                      });
                    }}
                  >
                    {contextMenuTarget.external ? "Open link" : "Open in new tab"}
                  </ContextMenuItem>
                  <ContextMenuItem
                    onSelect={() => {
                      if (!contextMenuTarget) return;
                      const text = contextMenuTarget.hash
                        ? `${contextMenuTarget.path}${contextMenuTarget.hash}`
                        : contextMenuTarget.path;
                      navigator.clipboard?.writeText(text).catch(() => {
                        /* clipboard unavailable */
                      });
                    }}
                  >
                    Copy link
                  </ContextMenuItem>
                </>
              ) : (
                <ContextMenuItem onSelect={() => setFindVisible(true)}>
                  Find in document
                </ContextMenuItem>
              )}
            </ContextMenuContent>
          </ContextMenu>
        </div>
      </div>

      {/* Quick-open file search */}
      <QuickOpenDialog
        open={quickOpenVisible && isActiveView}
        loading={quickOpenLoading || fullTreeLoading}
        error={quickOpenError ?? fullTreeError}
        emptyLabel={quickOpenError ?? fullTreeError ?? "No files found"}
        onClose={() => setQuickOpenVisible(false)}
        onSelect={(fullPath) =>
          // SW-FX-49: ⌘P is a fast peek — open the file (+ reveal the viewer in
          // console mode) and nothing else. It intentionally does NOT boot the
          // story's CLI workspace; that stays a deliberate gesture (⌘K palette).
          // Logic lives in a store-slice-narrowed helper so the workspace
          // side-effect is unreachable from here by construction.
          handleQuickOpenFileSelect(useViewerStore.getState(), teamKey, navRepoPath, fullPath)
        }
        items={effectiveQuickOpenFiles.map((f) => ({
          id: f.fullPath,
          label: f.name,
          sublabel: f.relativePath,
        }))}
      />

      {/* SW-EW-2 AC-5: quick-switch workspace picker (panel search affordance). */}
      <QuickOpenDialog
        open={workspacePaletteOpen && isActiveView}
        onClose={() => setWorkspacePaletteOpen(false)}
        onSelect={handleWorkspacePickerSelect}
        items={workspacePickerItems}
        placeholder="Search workspaces…"
        emptyLabel="No workspaces found"
      />

      {/* SW-SN-3: the Cmd+O note quick-switcher — the keyboard accelerator over
            the SN-2 rail's list. Extracted into its own leaf so its `notes`
            subscription re-renders only the mount, not this whole view. Gated on
            isActiveView (like the palette) so multiple team views don't stack. */}
      <NoteSwitcherMount isActiveView={isActiveView} />

      {/* SW-SN-6: the Cmd+Shift+O go-to-heading jump — the keyboard sibling of
            the note switcher, over the active note's headings. Same leaf pattern
            and isActiveView gate; the panel (which owns the editor) performs the
            scroll via the store jump signal. */}
      <HeadingJumpMount isActiveView={isActiveView} />
      <WorktreeSetupApprovalModal
        request={setupApprovalRequest}
        onApprove={() => resolveWorktreeSetupApproval(true)}
        onDecline={() => resolveWorktreeSetupApproval(false)}
      />
      <NoLocalCloneModal
        // T.4 AC-3 / Phase 10 review (Round 2 HIGH #1): construct a
        // synthetic-shaped repoPath from the prompt's `{teamKey,
        // sourceRepoSlug}` so the modal displays the slug the
        // PROMPT was raised for, not the panel's own slug. M.4
        // cross-repo: prompt for `acme/api`, panel for `acme/web`
        // — without this, the modal would display "acme/web" and
        // AttachLocalCloneDialog would target the wrong repo.
        repoPath={
          viewIdentity && noClonePromptRepo && noClonePromptRepo.teamKey === viewIdentity.teamKey
            ? buildSyntheticRepoPathFromPrompt(noClonePromptRepo)
            : null
        }
        onDismiss={dismissNoClonePrompt}
      />
      {/* The team's right pane (Inbox, notes and custom panes), drawn in the workbench's right
            column through this team's own host, so its state and this view's context (the story
            launch provider) carry across team switches. */}
      {rightPaneHost &&
        createPortal(
          <RightPaneLayout
            // Notes and content stay team-owned when source/agent focus changes.
            repoPath={navRepoPath}
            onClose={() => toggleRightPaneWithFocus(teamKey)}
            onFileSelect={handleFileSelect}
            fullTree={fullPipelineTree}
          />,
          rightPaneHost,
        )}
    </div>
  );
  // One-action "Launch agent" for this team's nav rows (the right pane portals
  // keep this context).
  return (
    <OwningScopeProvider scope={teamKey}>
      <StoryAgentLaunchProvider teamKey={teamKey}>{view}</StoryAgentLaunchProvider>
    </OwningScopeProvider>
  );
}

/** The redline a tab on its Changes renders: against GitHub, or one commit's change. */
function redlineFor(
  diff: TabDiff | undefined,
  base: {
    orgRoot: string;
    teamKey: string;
    navRepoPath: string;
    oldPath?: string;
    active: boolean;
  },
) {
  if (diff && diff !== true) return { ...base, commitSha: diff.sha, oldPath: diff.oldPath };
  return base;
}
