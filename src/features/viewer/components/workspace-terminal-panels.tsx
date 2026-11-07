// SPDX-License-Identifier: MIT
import { useTerminalThemeConfig } from "@/themes/use-terminal-theme";
import { beginContextNavigation } from "@/shared/lib/context-navigation";
import { recordContextChange } from "@/features/viewer/lib/context-switch";
import { useCliDndId, useCliDragStale } from "../lib/cli-drag-scope";
import { requestNewWorkspaceTerminal } from "../lib/new-workspace-terminal";
import { RemoteWorkspaceOffline } from "@/features/machines/components/remote-workspace-offline";
import { NewTerminalChooser } from "./new-terminal-chooser";
import { RemoteTerminalPane } from "./remote-terminal-pane";
import { PendingRemoteOpens } from "./pending-remote-opens";
import { workspaceKey } from "@/shared/lib/workspace-key";
import {
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import {
  effectiveMosaicRoot,
  getBackgroundSetupStepOutput,
  getViewerState,
  selectActiveTerminalTab,
  selectExpandedWorkspaceId,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import type { Workspace } from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import {
  useWorkspaceTabsSlice,
  useWorkspaceActiveTab,
  useWorkspacePanedTabIds,
  useShouldRenderTabGrid,
  useIsExpandedWorkspace,
  useWorkspaceTabGridPreset,
  useTabGridLayout,
  useTerminalFontSize,
  useTerminalFontSizePx,
  useFocusedWorkspaceId,
  useTrayMode,
  useSelectedRailKey,
  useRailKeyIndex,
  useFlatForegroundLayout,
  useRailForegroundLayout,
  useTeamTileOrder,
  useTerminalLayoutHydrated,
  useTerminalHostsUnreachable,
  useLocalTerminalServerReconnecting,
  useHeldTerminalOpens,
  useTerminalVisible,
  useExpandedWorkspaceId,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import type { PipelineStatus } from "@/shared/lib/pipeline-graph/types";
import { ShellTerminalPanel } from "./shell-terminal-panel";
import { MosaicView } from "./mosaic-view";
import { MIN_CELL_WIDTH_PX, MIN_CELL_HEIGHT_PX } from "../lib/tab-grid-layout";
import { TerminalTileChrome } from "./terminal-tile-chrome";
import type { SelectTabOptions } from "./terminal-tab-bar";
import { TileNeedsYouSlot } from "./tile-needs-you-slot";
import { TerminalGridCaption } from "./terminal-grid-caption";
import { StoryHud } from "./story-hud";
import { InboxSplit, useInboxTabCleanup } from "@/features/inbox/components/inbox-split";
import { SourceDiffOverlay } from "./source-diff-overlay";
import { WorktreeEditorSurface } from "./source-editor-inset";
import { foregroundTilesFor, isSplitActive, epicForegroundTiles } from "../lib/foreground-tiles";
import { getKindCapabilities } from "@/shared/lib/workspace-kind-capabilities";
import { workspaceDisplayLabel } from "@/shared/lib/worktree";
import { findLeafPath, placementChangesLayout, reconcilePresetTree } from "../lib/mosaic-tree";
import { useTerminalLives } from "../hooks/use-active-workspace-terminal-tabs";
import { type TerminalThemeConfig } from "./terminal-constants";
import {
  TerminalHeldNotice,
  TerminalHostsReconnecting,
  TerminalReconnectingBanner,
  TerminalServerNotice,
  useLocalServerUnreachable,
} from "@/features/shell/components/terminal-server-notice";
import { useTerminalServer } from "@/features/shell/lib/terminal-server";
import {
  closeTab,
  closeWorkspace,
  endTabs,
  hideTab,
  hideWorkspace,
  restartTab,
} from "../lib/terminal-close";
import { harnessPaneKey } from "../lib/harness-pane-key";
import { warmTerminalTabIds } from "../lib/terminal-warm-tabs";
import {
  showOneTab,
  newTerminalTab,
  showAllTabs,
  splitTerminal,
  zoomTerminalCell,
} from "../lib/terminal-tab-keys";
import { activateAndFocus, currentActivationGeneration } from "../lib/focus-cascade";
import {
  FOCUS_PANE_ATTR,
  FOCUS_TILE_ATTR,
  focusTileKey,
  usePaneFocusIndicator,
  useTileFocusIndicator,
} from "../lib/focus-indicator";
import { getEffectiveTerminalFontPx } from "@/shared/lib/font-sizes";
import { useTeamWorkspaces } from "@/shared/hooks/use-team-workspaces";
import { useIsConsole } from "@/shared/hooks/use-is-console";
import { useWorkspaceOverlayState } from "./workspace-overlay-state";
import {
  SetupFailureActions,
  SetupFailureDetails,
  SetupLineView,
  useDisplayedLine,
  WorkspaceAnnouncer,
  WorkspaceSetupLine,
} from "./setup-line";
import { describeSetup, type WorkspaceSetupState } from "../lib/workspace-setup-state";
import { WorkspaceKeptModified } from "./kept-modified-note";
import { useOperationAnnouncement } from "@/features/shell/components/operation-chip";
import { ContextLandingChip } from "./context-landing-chip";
import {
  AlertCircle,
  Check,
  ChevronRight,
  Circle,
  Clock,
  Loader2,
  RotateCw,
  X,
} from "lucide-react";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { DotPulse } from "ldrs/react";
import "ldrs/react/DotPulse.css";
import { useDroppable, useDraggable, useDndMonitor, type Active } from "@dnd-kit/core";
import {
  TERMINAL_GRID_DROPPABLE_ID,
  TILE_EDGE_SIDES,
  tileEdgePlacement,
  tileZoneId,
  unwrapDragWorkspaceId,
  type TileDropSide,
} from "../lib/terminal-drag";

/** SW-FX-52: stable empty ids array so the not-zoomed `hiddenTileIds` memo keeps
 *  a constant identity (no churn into the scalar aggregate selector). */
const EMPTY_WS_IDS: string[] = [];

/** The per-workspace tab slice (shared by a tile's chrome and its terminal
 *  body so the two subscriptions aren't duplicated inline). */
function useWorkspaceTabs(teamKey: string, workspaceId: string) {
  const tabs = useWorkspaceTabsSlice(teamKey, workspaceId);
  const activeTabId = useWorkspaceActiveTab(teamKey, workspaceId);
  return { tabs, activeTabId };
}

// ---------------------------------------------------------------------------
// WorkspaceTerminalPanels — SW-EW-3/4/17 split-terminal tiling.
//
// Renders the foreground as a MOSAIC TREE (SW-EW-17): a leaf is one workspace,
// a depth-1 split is the flat columns/rows of SW-EW-3/4 (rendered through the
// unchanged TerminalTileGroup), a nested split is the 2D mosaic. Membership is
// the open-workspace set (derived); the *arrangement* tree is reconciled to it
// on read via `effectiveMosaicRoot`.
//
// A tile that MOVES across the tree simply remounts its body in the new leaf:
// every terminal display is owned by the terminal manager, keyed by session, and
// a remounted slot re-attaches the SAME display (nothing is rebuilt).
// ---------------------------------------------------------------------------

interface WorkspaceTerminalPanelsProps {
  /** The team whose workspaces the CLI shows. It can differ from the active team:
   *  the CLI keeps the team it last moved to when the active team changes. */
  teamKey: string;
  /** Whether the CLI column is on screen (the active team's shown/hidden flag).
   *  Its terminals are visible exactly when it is, whichever team they belong to.
   *  The team view passes it; without it, the same rule read from the store. */
  shown?: boolean;
  /** This team's grid is hidden behind the shown team's (a warm team): its terminals are not
   *  visible, yet its panes stay mounted so coming back attaches nothing. */
  teamHidden?: boolean;
  /** T.6: source repo identity for the workspace's terminal cwd. */
  repoPath: string;
  /** SW-EW-12 AC-11: double-clicking a split-tile's story-id opens the story doc
   *  (reuses `openFile`). Restores the
   *  behavior the retired flat WorkspaceBar pill carried. */
  onOpenStoryDoc?: (storyFilePath: string) => void;
  /** SW-EW-18 AC-6: set a story tile's lifecycle status from the identity
   *  dropdown (the `WorkspacePill` pattern, plumbed through to the tile chrome). */
  onStoryStatusChange?: (workspaceId: string, newStatus: PipelineStatus) => void;
}

/** Context threaded through the recursive render so leaves can render their
 *  chrome and terminal body without prop-drilling. */
interface RenderShared {
  teamKey: string;
  /** The CLI column is on screen: its terminals are visible. */
  shown: boolean;
  /** See {@link WorkspaceTerminalPanelsProps.teamHidden}. */
  teamHidden: boolean;
  /** T.6: synthetic repo key for the active workspace — drives the file-refresh
   *  live-reload signal the SW-EW-28 expanded HUD shares (SW-SC-4 gate). */
  repoPath: string;
  /** SW-EW-18 AC-2: ≥2 foreground tiles → window controls (zoom/minimize) +
   *  drag handle are live; at N=1 the identity chrome still renders but those
   *  controls are omitted (nothing to zoom/rearrange against). */
  split: boolean;
  /** SW-EW-34 v2 (D8): console mode hosts a top workspace switcher that owns
   *  identity, so a lone (N=1) tile's `[status] ID` block is redundant —
   *  dropped when `isConsole && !split`. Split (N≥2) keeps the id (the top
   *  switcher marks only one focused chip, so tiles still disambiguate). */
  isConsole: boolean;
  workspaceById: Map<string, Workspace>;
  focusedWorkspaceId: string | undefined;
  themeConfig: TerminalThemeConfig;
  effectiveZoomId: string | null;
  hiddenTileCount: number;
  hiddenTileIds: readonly string[];
  terminalFontPx: number;
  closeWorkspace: (teamKey: string, workspaceId: string) => void;
  toggleZoom: (teamKey: string, tileId: string) => void;
  /** SW-EW-12 AC-11: double-click the tile's story-id → open doc + recenter. */
  onOpenStoryDoc?: (storyFilePath: string) => void;
  /** SW-EW-18 AC-6: set a story tile's lifecycle status. */
  onStoryStatusChange?: (workspaceId: string, newStatus: PipelineStatus) => void;
  /** SW-EW-33: the workspace id currently being dragged (tray chip unwrapped), or
   *  null when no drag is in progress. Drives the per-tile edge-zone mounting. */
  draggedWorkspaceId: string | null;
  /** SW-EW-33: zone ids (`tile:{ws}:{side}`) whose drop would be a no-op for the
   *  CURRENT drag — suppressed so a highlighted zone always yields a real move.
   *  Empty when nothing is dragging / a minimized chip is dragged (always adds). */
  suppressedZoneIds: ReadonlySet<string>;
}

/** The tile chrome's zoom: the expanded tile goes back to the grid; any other tile is focused
 *  and expanded. */
function toggleTileZoom(teamKey: string, tileId: string): void {
  const s = getViewerState();
  if (selectExpandedWorkspaceId(s, teamKey) === tileId) s.setCliExpanded(false);
  else s.expandWorkspace(teamKey, tileId);
}

export function WorkspaceTerminalPanels({
  teamKey,
  shown: shownProp,
  teamHidden = false,
  repoPath,
  onOpenStoryDoc,
  onStoryStatusChange,
}: WorkspaceTerminalPanelsProps) {
  const workspaces = useTeamWorkspaces(teamKey);
  const shownByStore = useTerminalVisible();
  const shown = shownProp ?? shownByStore;
  const terminalFontSize = useTerminalFontSize();
  const terminalFontSizePx = useTerminalFontSizePx();
  const terminalFontPx = getEffectiveTerminalFontPx(terminalFontSize, terminalFontSizePx);
  const terminalThemeName = useTerminalThemeConfig();
  const themeConfig = terminalThemeName;

  // Store actions have a stable identity for the store's lifetime, so reading
  // them once per render off the imperative door is equivalent to the previous
  // reactive `useViewerStore((s) => s.action)` subscriptions (which never fired
  // — the ref never changes) and keeps them out of the reactive read surface.
  const { closeWorkspace, setForegroundNodeSizes } = getViewerState();
  const focusedWorkspaceId = useFocusedWorkspaceId(teamKey);
  const isConsole = useIsConsole();
  // SW-EW-40: in grouped tray mode the mosaic renders ONLY the selected epic's
  // tiles, in that rail's own arrangement; flat mode is byte-identical (the
  // per-team slice + the full foregroundTilesFor set). Both read from the store
  // rail-key index — the tree never enters this component (the index carries the
  // tray's grouping, so the mosaic stays tree-free and shares ONE epic map).
  const grouped = useTrayMode(teamKey) === "grouped";
  const selectedRailKey = useSelectedRailKey(teamKey);
  const railIndex = useRailKeyIndex(teamKey);
  const flatLayout = useFlatForegroundLayout(teamKey);
  const railLayout = useRailForegroundLayout(teamKey, selectedRailKey);
  const layout = grouped ? railLayout : flatLayout;
  // Expanded is one setting for every team and always zooms the focused workspace (D68).
  const expandedTileId = useExpandedWorkspaceId(teamKey);

  // The tile set (matches `scopedForegroundTiles`, the store resolver's shared
  // source): grouped mode WITH a built rail index → the selected epic's slice;
  // flat mode OR the pre-index cold-start window (`railIndex === undefined`, a
  // restart into grouped mode before the tree loads) → the full foreground set, so
  // terminals render immediately instead of flashing empty until the index lands.
  // A workspace outside the selected epic renders no tile: its terminal displays
  // stay parked in the terminal manager, with the PTY alive, until it returns.
  // The tiles follow the team's shared workspace order, as its pills do.
  const tileOrder = useTeamTileOrder(teamKey);
  const tiles = useMemo(
    () =>
      grouped && railIndex !== undefined
        ? epicForegroundTiles(workspaces, railIndex, selectedRailKey, tileOrder)
        : foregroundTilesFor(workspaces, tileOrder),
    [grouped, workspaces, railIndex, selectedRailKey, tileOrder],
  );
  const split = isSplitActive(tiles.length);

  // The reconciled arrangement: derives flat/grid from the preset when there's
  // no custom mosaic, then prunes/appends to match the live tile set.
  const root = useMemo(
    () =>
      effectiveMosaicRoot(
        layout,
        tiles.map((w) => w.id),
      ),
    [layout, tiles],
  );

  // SW-EW-33: track the dragged workspace via the dnd MONITOR (drag start/end
  // callbacks), NOT `useDndContext()` — the context value changes on every
  // pointermove, which would re-render this whole panel (and its tiles) each
  // frame of a drag. The monitor's listener is stable, so this re-renders only
  // when a drag begins or ends.
  const [draggedWorkspaceId, setDraggedWorkspaceId] = useState<string | null>(null);
  const dragMonitor = useMemo(
    () => ({
      onDragStart: ({ active }: { active: Pick<Active, "id"> }) =>
        setDraggedWorkspaceId(unwrapDragWorkspaceId(active.id)),
      onDragEnd: () => setDraggedWorkspaceId(null),
      onDragCancel: () => setDraggedWorkspaceId(null),
    }),
    [],
  );
  useDndMonitor(dragMonitor);

  // While a workspace is dragged, mark the drop zones whose placement would NOT
  // change the arrangement (e.g. dropping a tile onto the edge it already
  // occupies relative to its neighbor) so they don't highlight — every shown
  // zone is a real move. Minimized chips (not in the tree) always add, so nothing
  // is suppressed for them.
  const suppressedZoneIds = useMemo<ReadonlySet<string>>(() => {
    const set = new Set<string>();
    if (!draggedWorkspaceId || !root) return set;
    for (const t of tiles) {
      if (t.id === draggedWorkspaceId) continue; // own tile: zones already hidden
      for (const side of TILE_EDGE_SIDES) {
        const changes = placementChangesLayout(root, draggedWorkspaceId, {
          targetWorkspaceId: t.id,
          ...tileEdgePlacement(side),
        });
        if (!changes) set.add(tileZoneId(t.id, side));
      }
    }
    return set;
  }, [draggedWorkspaceId, root, tiles]);

  // R-5: a stale zoom id (its workspace closed) is treated as un-zoomed.
  const effectiveZoomId =
    expandedTileId && tiles.some((w) => w.id === expandedTileId) ? expandedTileId : null;
  const zoomPath = useMemo(
    () => (effectiveZoomId ? findLeafPath(root, effectiveZoomId) : null),
    [root, effectiveZoomId],
  );

  // SLW-FX-8: a zoomed CLI tile is a live terminal, so Esc is terminal input
  // (vim/less/fzf/any TUI) — never a restore path. The SW-EW-4 document-level
  // Esc-restore handler was deleted here: it could only guess (via event.target)
  // whether focus was inside .xterm, and that guess was wrong often enough to
  // yank the zoom out from under an active shell. Restore now lives ONLY on the
  // deliberate gestures — Cmd+Shift+M (registry), the ⛶ button, and the chrome
  // double-click — none of which can be typed by accident into a TUI.

  // AC-8 (zoom): highest-priority attention across the HIDDEN tiles, badged on
  // the zoomed tile so zooming never buries a notification. The tile chrome's
  // "+N hidden" mark subscribes to just the hidden tiles' sessions itself.
  const hiddenTileIds = useMemo(
    () =>
      effectiveZoomId
        ? tiles.filter((w) => w.id !== effectiveZoomId).map((w) => w.id)
        : EMPTY_WS_IDS,
    [effectiveZoomId, tiles],
  );
  const hiddenTileCount = effectiveZoomId ? tiles.length - 1 : 0;

  const workspaceById = useMemo(() => new Map(tiles.map((w) => [w.id, w])), [tiles]);

  // SW-EW-32: the epic HUD card now self-fetches its summary via
  // `useStoryDetails(epicDocPath, headings)` (honoring the configured headings),
  // so the old per-epic `## Purpose` tree-walk map is gone — both story and epic
  // tiles resolve their summary through the one hook.

  const shared: RenderShared = {
    teamKey,
    shown,
    teamHidden,
    repoPath,
    split,
    isConsole,
    workspaceById,
    focusedWorkspaceId,
    themeConfig,
    effectiveZoomId,
    hiddenTileCount,
    hiddenTileIds,
    terminalFontPx,
    draggedWorkspaceId,
    suppressedZoneIds,
    closeWorkspace,
    toggleZoom: toggleTileZoom,
    onOpenStoryDoc,
    onStoryStatusChange,
  };

  // PT-13: the whole grid stays a drop target for the empty-area / center
  // fallback; SW-EW-17 adds per-tile edge zones inside each frame.
  const { setNodeRef, isOver } = useDroppable({
    id: useCliDndId(TERMINAL_GRID_DROPPABLE_ID),
    disabled: useCliDragStale(),
  });

  return (
    <div
      ref={setNodeRef}
      className={`flex-1 min-h-0 relative${isOver ? " outline outline-2 outline-accent outline-offset-[-2px] rounded" : ""}`}
      data-drop-active={isOver ? "true" : "false"}
    >
      {/* Frame layer: the mosaic tree (resizable panels + chrome + drop zones).
          Split-vs-1-up now rides in `shared.split`. */}
      <MosaicView
        resizeIdentity={teamKey}
        node={root}
        zoomPath={zoomPath}
        onSizesCommit={(path, sizes) => setForegroundNodeSizes(teamKey, path, sizes)}
        renderLeaf={(id) => {
          const ws = workspaceById.get(id);
          // Each team's grid is its own subtree (the CLI keeps one per warm team), so the
          // workspace id alone identifies a tile and a team switch never remounts it.
          return ws ? <WorkspaceTileFrame key={id} workspace={ws} shared={shared} /> : null;
        }}
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// WorkspaceErrorPlaceholder — shown for stale/error workspaces (F-18 AC-3)
// ---------------------------------------------------------------------------

interface WorkspaceErrorPlaceholderProps {
  workspaceId: string;
  statusError?: string;
  onClose: () => void;
  /** SW-XS-10: a failed Step 1 — the failure line with Retry and Details
   *  replaces the bare message. */
  setupFailure?: { ws: Workspace; teamKey: string };
}

function WorkspaceErrorPlaceholder({
  workspaceId,
  statusError,
  onClose,
  setupFailure,
}: WorkspaceErrorPlaceholderProps) {
  const failed =
    setupFailure?.ws.setupState?.kind === "failed" ? setupFailure.ws.setupState : undefined;
  const view = failed ? describeSetup(failed) : null;
  return (
    <div
      className="h-full flex items-center justify-center"
      data-testid={`workspace-error-${workspaceId}`}
    >
      <div className="flex flex-col items-center gap-3 text-center max-w-md px-6">
        {failed && view && setupFailure ? (
          <SetupLineView
            view={view}
            variant="pane"
            testId={`workspace-setup-failed-${workspaceId}`}
            actions={
              <SetupFailureActions
                ws={setupFailure.ws}
                teamKey={setupFailure.teamKey}
                state={failed}
              />
            }
            details={<SetupFailureDetails state={failed} />}
          />
        ) : (
          <>
            <AlertCircle size={32} className="text-red-400" />
            <p className="text-sm text-[var(--foreground)]">
              {statusError ?? "This workspace is no longer available"}
            </p>
          </>
        )}
        <div className="flex items-center gap-2">
          <button
            onClick={onClose}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs rounded border border-[var(--border)] text-[var(--muted-foreground)] hover:text-[var(--foreground)] hover:bg-[var(--accent)] transition-colors"
          >
            <X size={12} />
            Close tab
          </button>
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// WorkspaceMaterializeError — SLW-CO-7 (AC-6): the in-pane hand-off when a
// materialization wave FAILS. An opaque absolute-inset cover over the (warm)
// terminal wrapping the shared `WorkspaceErrorPlaceholder`. Recovery is close +
// reopen (SW-XS-3: nothing re-syncs a workspace on its own).
// ---------------------------------------------------------------------------

interface WorkspaceMaterializeErrorProps {
  workspaceId: string;
  teamKey: string;
  reason?: string;
  themeConfig: TerminalThemeConfig;
}

function WorkspaceMaterializeError({
  workspaceId,
  teamKey,
  reason,
  themeConfig,
}: WorkspaceMaterializeErrorProps) {
  return (
    <div
      className="absolute inset-0 z-10"
      style={{ backgroundColor: themeConfig.theme.background }}
      data-testid="workspace-materialize-failed"
    >
      <WorkspaceErrorPlaceholder
        workspaceId={workspaceId}
        statusError={reason ?? "Context couldn't be prepared"}
        onClose={() => getViewerState().closeWorkspace(teamKey, workspaceId)}
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// WorkspaceSetupBadge — RF-perf-14 (decouple): a small, NON-blocking corner chip
// over the LIVE terminal while the worktree's `setup:` commands run (or failed)
// in the background. Deliberately NOT a full-inset cover (unlike
// WorkspaceMaterializeError) — the shell stays usable throughout. On failure it
// offers Retry (re-runs the backgrounded setup) without hiding the terminal.
// ---------------------------------------------------------------------------

// SW-WT-12 (S2): the per-command setup checklist rows — queued → running → ✓/✗ —
// rendered inside the non-blocking badge over the live terminal (Sally's spec).
type SetupStep = NonNullable<
  Extract<NonNullable<Workspace["backgroundSetup"]>, { status: "running" }>["steps"]
>[number];

function SetupStepRow({ step, workspaceId }: { step: SetupStep; workspaceId: string }) {
  // SW-WT-12 (S2): output on demand — read a snapshot only while expanded, so a
  // chatty step never re-renders (the buffer is non-reactive). Queued steps have
  // nothing to show yet, so they get no disclosure.
  const [open, setOpen] = useState(false);
  const canExpand = step.status !== "queued";
  const output = open ? getBackgroundSetupStepOutput(workspaceId, step.index) : undefined;
  const icon =
    step.status === "done" ? (
      <Check size={12} className="shrink-0 text-green-500" aria-hidden />
    ) : step.status === "failed" ? (
      <AlertCircle size={12} className="shrink-0 text-[var(--cs-warning-text)]" aria-hidden />
    ) : step.status === "running" ? (
      <Loader2
        size={12}
        className="shrink-0 animate-spin text-[var(--cs-status-busy)]"
        aria-hidden
      />
    ) : (
      <Circle size={12} className="shrink-0 text-muted-foreground" aria-hidden />
    );
  const muted = step.status === "queued" || step.status === "done";
  return (
    <li aria-label={`step ${step.index + 1}, ${step.command}, ${step.status}`}>
      <div
        className={`flex items-center gap-1.5 ${muted ? "text-[var(--muted-foreground)]" : "text-[var(--foreground)]"}`}
      >
        {icon}
        <span className="min-w-0 flex-1 truncate font-mono">{step.command}</span>
        {step.status === "failed" && step.exitCode != null && (
          <span className="shrink-0 whitespace-nowrap text-[var(--cs-warning-text)]">{`exit ${step.exitCode}`}</span>
        )}
        {canExpand && (
          <button
            type="button"
            aria-expanded={open}
            aria-label={`${open ? "Hide" : "Show"} output for ${step.command}`}
            onClick={() => setOpen((o) => !o)}
            className="shrink-0 rounded p-0.5 text-[var(--muted-foreground)] hover:text-[var(--foreground)]"
          >
            <ChevronRight size={12} className={open ? "rotate-90 transition-transform" : ""} />
          </button>
        )}
      </div>
      {open && (
        <div className="mt-1 pl-[18px]">
          <div className="text-[10px] text-[var(--muted-foreground)]">
            Output may contain secrets. Review before sharing.
          </div>
          <pre className="mt-0.5 max-h-28 overflow-auto whitespace-pre-wrap break-words text-[var(--foreground)] select-text">
            {output && output.length > 0 ? output : "No output captured."}
          </pre>
        </div>
      )}
    </li>
  );
}

function SetupChecklist({ steps, workspaceId }: { steps: SetupStep[]; workspaceId: string }) {
  if (steps.length === 0) return null;
  return (
    <ol
      className="pointer-events-auto mt-1 max-h-40 w-full max-w-sm space-y-0.5 overflow-auto rounded border border-[var(--border)] bg-[var(--popover)]/95 px-2 py-1 text-xs shadow-sm"
      data-testid="workspace-setup-checklist"
    >
      {steps.map((s) => (
        <SetupStepRow key={s.index} step={s} workspaceId={workspaceId} />
      ))}
    </ol>
  );
}

function WorkspaceSetupBadge({
  workspaceId,
  teamKey,
  state,
}: {
  workspaceId: string;
  teamKey: string;
  state: NonNullable<Workspace["backgroundSetup"]>;
}) {
  const [detailsOpen, setDetailsOpen] = useState(false);
  const detailsButtonRef = useRef<HTMLButtonElement>(null);
  const detailsId = useId();
  const closeDetails = () => {
    setDetailsOpen(false);
    detailsButtonRef.current?.focus();
  };

  if (state.status === "queued") {
    // RF-perf-14d: waiting behind the background-setup concurrency gate — reads as
    // waiting (dimmed clock, NO spinner) so a queued worktree doesn't look like
    // it's actively installing. Flips to the running chip once a slot is acquired.
    // SW-WT-12 (S2): show the seeded checklist (all queued) below the chip.
    return (
      <div className="absolute bottom-2 left-2 z-10 flex flex-col items-start gap-1">
        <div
          className="flex items-center gap-1.5 rounded border border-[var(--border)] bg-[var(--popover)]/90 px-2 py-1 text-xs text-[var(--muted-foreground)]/70 shadow-sm pointer-events-none"
          data-testid={`workspace-setup-queued-${workspaceId}`}
        >
          <Clock size={12} className="shrink-0" />
          <span className="max-w-[18rem] truncate">Queued…</span>
        </div>
        {state.steps && <SetupChecklist steps={state.steps} workspaceId={workspaceId} />}
      </div>
    );
  }
  if (state.status === "running") {
    return (
      <div className="absolute bottom-2 left-2 z-10 flex flex-col items-start gap-1">
        <div
          className="flex items-center gap-1.5 rounded border border-[var(--border)] bg-[var(--popover)]/90 px-2 py-1 text-xs text-[var(--muted-foreground)] shadow-sm pointer-events-none"
          data-testid={`workspace-setup-running-${workspaceId}`}
        >
          <Clock size={12} className="shrink-0" aria-hidden />
          <span className="max-w-[18rem] truncate">{state.label ?? "Setting up…"}</span>
        </div>
        {state.steps && <SetupChecklist steps={state.steps} workspaceId={workspaceId} />}
      </div>
    );
  }
  const diagnostics = state.diagnostics;
  const failed = diagnostics?.failedResult;
  const noCapturedOutput = failed != null && !failed.stdout && !failed.stderr;

  return (
    <div className="pointer-events-none absolute inset-x-2 bottom-2 z-10 flex max-h-[calc(100%-1rem)] flex-col items-start text-xs">
      {detailsOpen && (
        <section
          id={detailsId}
          role="region"
          aria-label="Setup failure details"
          tabIndex={-1}
          onKeyDown={(event) => {
            if (event.key !== "Escape") return;
            event.stopPropagation();
            closeDetails();
          }}
          className="pointer-events-auto mb-1 min-h-0 w-full max-w-xl overflow-auto rounded border border-[#d29922]/50 bg-[var(--popover)] p-3 text-[var(--foreground)] shadow-lg"
          data-testid={`workspace-setup-details-${workspaceId}`}
        >
          <h3 className="mb-2 font-medium">Setup failed</h3>
          <p className="mb-2 text-[var(--muted-foreground)]">
            Setup output may contain secrets. Review before copying or sharing.
          </p>
          {failed ? (
            <div className="space-y-2">
              <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-1">
                <dt className="text-[var(--muted-foreground)]">Step</dt>
                <dd>{`${failed.phase} · ${failed.step_index + 1}`}</dd>
                <dt className="text-[var(--muted-foreground)]">Exit code</dt>
                <dd>{failed.exit_code ?? "Unavailable"}</dd>
              </dl>
              <div>
                <div className="text-[var(--muted-foreground)]">Command</div>
                <pre className="overflow-x-auto whitespace-pre text-[inherit] select-text">
                  {failed.command}
                </pre>
              </div>
              {failed.stdout && (
                <SetupDiagnosticStream
                  label="Standard output"
                  text={failed.stdout}
                  metadata={diagnostics.streams.stdout}
                />
              )}
              {failed.stderr && (
                <SetupDiagnosticStream
                  label="Standard error"
                  text={failed.stderr}
                  metadata={diagnostics.streams.stderr}
                />
              )}
              {noCapturedOutput && (
                <p className="text-[var(--muted-foreground)]">No captured output</p>
              )}
            </div>
          ) : (
            <pre className="whitespace-pre-wrap break-words text-[inherit] select-text">
              {state.error}
            </pre>
          )}
        </section>
      )}
      <div
        className="pointer-events-auto flex max-w-full items-center gap-2 rounded border border-[#d29922]/50 bg-[var(--popover)]/95 px-2 py-1 shadow-sm"
        data-testid={`workspace-setup-failed-${workspaceId}`}
        aria-live="polite"
      >
        <AlertCircle size={12} className="shrink-0 text-[var(--cs-warning-text)]" aria-hidden />
        <span className="max-w-[18rem] truncate text-[var(--foreground)]">Setup failed</span>
        <button
          ref={detailsButtonRef}
          type="button"
          aria-expanded={detailsOpen}
          aria-controls={detailsId}
          onClick={() => setDetailsOpen((open) => !open)}
          onKeyDown={(event) => {
            if (event.key !== "Escape" || !detailsOpen) return;
            event.stopPropagation();
            closeDetails();
          }}
          className="rounded border border-[var(--border)] px-1.5 py-0.5 text-[var(--muted-foreground)] transition-colors hover:bg-[var(--accent)] hover:text-[var(--foreground)] focus-visible:outline focus-visible:outline-2"
        >
          Details
        </button>
        <button
          type="button"
          onClick={(event) => {
            event.stopPropagation();
            setDetailsOpen(false);
            getViewerState().retryWorktreeSetup(teamKey, workspaceId);
          }}
          data-testid={`workspace-setup-retry-${workspaceId}`}
          className="flex items-center gap-1 rounded border border-[var(--border)] px-1.5 py-0.5 text-[var(--muted-foreground)] transition-colors hover:bg-[var(--accent)] hover:text-[var(--foreground)] focus-visible:outline focus-visible:outline-2"
        >
          <RotateCw size={11} aria-hidden />
          Retry
        </button>
      </div>
    </div>
  );
}

function SetupDiagnosticStream({
  label,
  text,
  metadata,
}: {
  label: string;
  text: string;
  metadata: { readonly originalLength: number; readonly truncated: boolean };
}) {
  return (
    <div>
      <div className="text-[var(--muted-foreground)]">{label}</div>
      {metadata.truncated && (
        <div className="text-[var(--muted-foreground)]">
          {`Truncated from ${metadata.originalLength.toLocaleString()} characters`}
        </div>
      )}
      <pre className="max-h-40 overflow-auto whitespace-pre-wrap break-words text-[inherit] select-text">
        {text}
      </pre>
    </div>
  );
}

// ---------------------------------------------------------------------------
// WorkspaceTileFrame — one mosaic leaf: (optional) chrome + the terminal body +
// directional drop zones. Holds NO terminal/PTY state — displays live in the
// terminal manager, so a leaf remount on rearrange rebuilds nothing.
// ---------------------------------------------------------------------------

interface WorkspaceTileFrameProps {
  workspace: Workspace;
  shared: RenderShared;
}

/** A chrome press on one of these keeps its own focus (buttons, menus, the tab
 *  strip, the rename input…); anywhere else in the chrome focuses the terminal. */
const CHROME_CONTROL_SELECTOR =
  'button, a, input, textarea, select, [contenteditable], [tabindex], [role="tab"], [role="menu"], [role="menuitem"]';

/** SW-EW-27: the single tile-focus primitive — make `workspaceId` the focused
 *  tile (`activeWorkspace`). Idempotent: a click on the already-focused tile is
 *  a true no-op (no store churn / focus flicker). The tile frame runs it for
 *  every press inside the tile, chrome and terminal body alike. Reads the store
 *  imperatively (a click-time side effect, not render state); `switchWorkspace`
 *  is a synchronous Zustand setter, so focus-follows-click never blocks the UI.
 *  Also run on focus capture, so keyboard focus that reaches a tile without a
 *  click (Tab, a menu returning focus) selects it too — the focus line and the
 *  tile that ⌘T/⌘W act on can't disagree. Never moves DOM focus, so no loop. */
function useFocusTile(teamKey: string, workspaceId: string): () => void {
  return useCallback(() => {
    const store = getViewerState();
    if (store.activeWorkspace[teamKey] !== workspaceId) {
      recordContextChange("nav", () => store.switchWorkspace(teamKey, workspaceId));
    }
  }, [teamKey, workspaceId]);
}

function WorkspaceTileFrame({ workspace: ws, shared }: WorkspaceTileFrameProps) {
  const { teamKey, split } = shared;
  // A warm team's hidden grid: its ids may repeat the shown team's, so it takes no drag.
  const hiddenGrid = useCliDragStale();
  const { tabs, activeTabId } = useWorkspaceTabs(teamKey, ws.id);
  // SW-EW-46: the tab-grid state drives the chrome's dial affordances (in-view
  // markers + the Grid-all/Collapse toggle). `showGrid` is expanded AND paned (D1);
  // `canGrid` is expanded (may grid) — resolved through the same `expandedWorkspaceId`
  // as the render, so the dial can't show while the grid wouldn't render.
  const panedTabIds = useWorkspacePanedTabIds(teamKey, ws.id);
  const showGrid = useShouldRenderTabGrid(teamKey, ws.id);
  const canGrid = useIsExpandedWorkspace(teamKey, ws.id);

  const isFocused = ws.id === shared.focusedWorkspaceId;
  const isZoomed = ws.id === shared.effectiveZoomId;

  const focusTile = useFocusTile(teamKey, ws.id);
  const tileKey = focusTileKey(teamKey, ws.id);
  const focusIndicator = useTileFocusIndicator(tileKey);

  // Each tab's inbox docks inside its own pane (see `renderPane`); the tile only
  // forgets a closed tab's remembered inbox width and lens.
  useInboxTabCleanup(tabs);

  // A press on the chrome selects the tile; one that isn't on a control also
  // moves keyboard focus into the tile's terminal, so the focus line (which
  // follows real focus) lands where the user clicked. Controls own their focus.
  const focusTileFromChrome = useCallback(
    (event: ReactMouseEvent) => {
      focusTile();
      // Scoped to the chrome itself: in a split its wrapper is the (focusable)
      // tile drag handle, which is not a control that owns focus.
      const control =
        event.target instanceof Element ? event.target.closest(CHROME_CONTROL_SELECTOR) : null;
      if (control && event.currentTarget.contains(control)) return;
      // Keep focus where it is until the terminal takes it (no one-frame dim of
      // the line); a pointer-driven tile drag is unaffected by a mousedown default.
      event.preventDefault();
      activateAndFocus({ teamKey, workspaceId: ws.id, yieldToEditing: true });
    },
    [focusTile, teamKey, ws.id],
  );

  const actions = useTileTabActions(teamKey, ws.id, focusTile);

  // AC-2: an on-screen tile is itself a drag source (rearrange) — the chrome bar
  // is the drag handle (like a window titlebar), so the terminal body stays
  // fully interactive. The 6px PointerSensor threshold keeps chrome button
  // clicks working. Only meaningful in split mode — `disabled` at N=1 (1-up has
  // nothing to rearrange against).
  const {
    attributes,
    listeners,
    setNodeRef: setDragRef,
    isDragging,
  } = useDraggable({
    id: useCliDndId(ws.id),
    disabled: !split || hiddenGrid,
  });

  return (
    <div
      ref={setDragRef}
      // SW-EW-33: directional pane focus (Cmd+Opt+arrows) reads each tile's
      // on-screen rect by this attribute to find the adjacent pane — works on any
      // nested-split arrangement, not just a grid.
      data-pane-id={ws.id}
      {...{ [FOCUS_TILE_ATTR]: tileKey }}
      // SW-EW-27: capture-phase focus for any press or focus inside the tile —
      // chrome, drop zones and the terminal body alike. `focusTile` is idempotent
      // and never calls preventDefault/stopPropagation, so xterm still focuses its
      // own textarea on the same pointerdown and click-drag selection is untouched.
      onPointerDownCapture={focusTile}
      onFocusCapture={focusTile}
      // `overflow-clip`, not `hidden`: the frame must never be a scroll
      // container. A focused xterm textarea (a newly opened remote tab's
      // autofocus) is scrolled into view through every scrollable ancestor —
      // `overflow:hidden` only blocks the wheel — which used to slide the
      // tile up and leave its tab bar under the workspace pills.
      className={`flex flex-col h-full min-h-0 relative overflow-clip ${isDragging ? "opacity-50" : ""}`}
    >
      {/* AC-2: identity chrome renders at N≥1 (including 1-up). The drag handle +
          zoom toggle are split-only (omitted at N=1). SW-EW-30: the
          minimize/Exit control is NOT split-gated anymore — it renders at every
          N so the last tile can be closed and the CLI drained to empty. */}
      <div
        {...(split ? listeners : {})}
        {...(split ? attributes : {})}
        className={split ? "cursor-grab active:cursor-grabbing" : undefined}
      >
        <TerminalTileChrome
          workspace={ws}
          teamKey={teamKey}
          isFocused={isFocused}
          focusIndicator={focusIndicator}
          // SW-EW-34 v2 (D8): drop the redundant id block on a lone console
          // tile — a console top switcher owns identity there.
          // SLW-FX-18: …but ONLY for kinds that have no per-tile identity to
          // show. The switcher that shipped to main (`IdentityMenu`)
          // carries org·team only — never the story title or status dropdown —
          // so a lone-console STORY tile was left with a blank top-left. Narrow
          // the gate with the same centralized `statusBadge` capability the
          // chrome uses to render the dropdown (`terminal-tile-chrome.tsx`), so
          // the mount decision and the show decision can't disagree: a story
          // (statusBadge=true) keeps its title + status; identity-less kinds
          // (epic/root/generic) stay hidden exactly as before.
          hideIdentity={
            shared.isConsole && !shared.split && !getKindCapabilities(ws.kind).statusBadge
          }
          tabs={tabs}
          activeTabId={activeTabId}
          chromeBg={shared.themeConfig.chromeBg}
          terminalBg={shared.themeConfig.theme.background}
          // Hide (–) and the default close (✕) are available at EVERY N —
          // including the lone tile (N=1); closing the last workspace drains the
          // CLI to its empty state. Zoom below stays split-only — it genuinely
          // has nothing to do with a single tile.
          onHideTile={() => hideWorkspace(teamKey, ws.id)}
          onCloseTile={() => void closeWorkspace(teamKey, ws.id)}
          onFocusTile={focusTileFromChrome}
          onToggleZoom={
            split
              ? () => {
                  focusTile();
                  shared.toggleZoom(teamKey, ws.id);
                }
              : undefined
          }
          isZoomed={isZoomed}
          hiddenTileCount={isZoomed ? shared.hiddenTileCount : 0}
          hiddenAttentionScope={isZoomed ? { teamKey, wsIds: shared.hiddenTileIds } : undefined}
          onOpenStoryDoc={shared.onOpenStoryDoc}
          onStoryStatusChange={
            shared.onStoryStatusChange
              ? (newStatus) => shared.onStoryStatusChange!(ws.id, newStatus)
              : undefined
          }
          // SW-EW-46: the tab-grid dial surface (only meaningful while expanded).
          panedTabIds={panedTabIds}
          gridActive={showGrid}
          canGrid={canGrid}
          {...actions}
        />
      </div>
      {/* SW-TP CAP-13: the standalone per-tab primitives toolbar is retired.
          Its contents (cwd, progress note, Editor/Diff/Inbox/HUD toggles) have
          moved into the per-tab TerminalGridCaption strip above each visible
          tab's terminal body. See mount in `renderPane` below. */}
      {/* The tile's "agent needs you" slot: a pending ask_user from one of this
          tile's tabs, between the chrome and the terminal (never over it). */}
      <TileNeedsYouSlot
        workspaceId={ws.id}
        tabs={tabs}
        activeTabId={activeTabId}
        onShowTab={actions.onSelectTab}
      />
      <div className="flex-1 min-h-0 relative" data-testid={`workspace-panels-${ws.id}`}>
        {ws.status === "error" ? (
          <WorkspaceErrorPlaceholder
            workspaceId={ws.id}
            statusError={ws.statusError}
            onClose={() => shared.closeWorkspace(teamKey, ws.id)}
            setupFailure={{ ws, teamKey }}
          />
        ) : (
          <div className="absolute inset-0">
            <WorkspaceTerminalBody
              workspace={ws}
              teamKey={teamKey}
              shown={shared.shown && (!shared.effectiveZoomId || isZoomed)}
              teamHidden={shared.teamHidden && (!shared.effectiveZoomId || isZoomed)}
              isFocused={isFocused}
              terminalFontPx={shared.terminalFontPx}
              themeConfig={shared.themeConfig}
            />
          </div>
        )}
        {/* RF-perf-14 (decouple): background `setup:` badge over the LIVE terminal
            (running / retryable-failed). Never rendered in the error state above
            (there's no shell to sit on then). */}
        {ws.status !== "error" && ws.backgroundSetup && (
          <WorkspaceSetupBadge workspaceId={ws.id} teamKey={teamKey} state={ws.backgroundSetup} />
        )}
        {/* SW-XS-10: the setup line over the LIVE terminal — the wait for the
            first agent, its Step 2, Ready, or a Step 2 failure with Retry. A
            corner chip like the setup badge: it never covers or resizes the
            terminal. (While creating, the init cover carries the line.) */}
        {ws.status === "ready" && (ws.setupState || ws.keptModified) && (
          <div
            className={`pointer-events-auto absolute left-2 z-10 flex max-w-[min(32rem,calc(100%-1rem))] flex-col items-start gap-1 ${
              ws.backgroundSetup ? "bottom-11" : "bottom-2"
            }`}
          >
            <WorkspaceSetupLine ws={ws} teamKey={teamKey} variant="corner" />
            {/* SW-XS-10 (§11.2): what the last preparation kept because it was
                edited here, with "Discard my edits". */}
            <WorkspaceKeptModified ws={ws} teamKey={teamKey} />
          </div>
        )}
        {/* SW-EW-28: per-tile story-context HUD floats over the terminal (z-10,
            below the z-20 drag bands). Visibility/size are per-workspace (the
            chrome toggle drives show/hide); the compact card shows the story
            summary / epic purpose, the expanded view the full doc. */}
        <StoryHud
          hostTeamKey={teamKey}
          workspace={ws}
          isFocused={isFocused}
          repoPath={shared.repoPath}
          zoomActive={shared.effectiveZoomId !== null}
        />
        {/* The per-tab source-diff review overlay — same per-tile mount as
            StoryHud, but render-gated on `isFocused` AND keyed on the active
            tab's sessionId (DESIGN-per-tab-primitives.md §3.2). */}
        <SourceDiffOverlay
          workspace={ws}
          teamKey={teamKey}
          activeTabSessionId={activeTabId}
          isFocused={isFocused}
          zoomActive={shared.effectiveZoomId !== null}
        />
        {/* SW-TP D1: the SourceFolderOverlay file-browse surface is retired.
            The Editor overlay now covers all folder-rooted tabs (CAP-1) and the
            standalone source browser was redundant with it. */}
        {/* The per-tab worktree code editor — opened from the toolbar, the
            ⌘⇧C hotkey, or the diff's "Open editor" button. A cheap gate:
            renders nothing until actually open (no watcher/reads idle). */}
        <WorktreeEditorSurface
          workspace={ws}
          teamKey={teamKey}
          activeTabSessionId={activeTabId}
          isFocused={isFocused}
        />
        <TileEdgeZones
          workspaceId={ws.id}
          draggedWorkspaceId={shared.draggedWorkspaceId}
          suppressedZoneIds={shared.suppressedZoneIds}
        />
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// TileEdgeZones — SW-EW-17 / SW-EW-33: the four edge drop bands overlaid on a
// tile, mounted ONLY while a drag is active (driven by `draggedWorkspaceId` from
// the parent's dnd monitor) so they never intercept terminal input otherwise. A
// drop resolves to `tile:{wsId}:{side}` (see terminal-drag).
// ---------------------------------------------------------------------------

// SW-EW-33: ONLY the four edges are valid drop targets — a drop splits the tile
// into a new adjacent pane. There is deliberately NO `center` zone: a tile owns
// one workspace (a group of tabbed terminals), so you cannot drop a workspace
// INTO another (no merge) — the center is an inert dead zone (the collision
// detector returns no hit there, so a center release snaps back).
const EDGE_ZONES: { side: TileDropSide; className: string }[] = [
  { side: "left", className: "left-0 top-0 bottom-0 w-1/4" },
  { side: "right", className: "right-0 top-0 bottom-0 w-1/4" },
  { side: "top", className: "top-0 left-1/4 right-1/4 h-1/4" },
  { side: "bottom", className: "bottom-0 left-1/4 right-1/4 h-1/4" },
];

function TileEdgeZones({
  workspaceId,
  draggedWorkspaceId,
  suppressedZoneIds,
}: {
  workspaceId: string;
  draggedWorkspaceId: string | null;
  suppressedZoneIds: ReadonlySet<string>;
}) {
  if (!draggedWorkspaceId) return null; // no drag in progress → no zones
  // SW-EW-33: don't offer drop zones on the tile of the workspace being dragged —
  // dropping a workspace onto its own tile is a no-op (you can't split/move it
  // against itself), so showing the affordance is misleading.
  if (draggedWorkspaceId === workspaceId) return null;
  // SW-EW-33: also drop any edge whose placement wouldn't change the arrangement
  // (e.g. dropping a tile onto the edge it already occupies) — a highlighted zone
  // is always a real move.
  const zones = EDGE_ZONES.filter((z) => !suppressedZoneIds.has(tileZoneId(workspaceId, z.side)));
  if (zones.length === 0) return null;
  return (
    <div className="absolute inset-0 z-20" data-testid={`tile-zones-${workspaceId}`}>
      {zones.map((z) => (
        <TileEdgeZone
          key={z.side}
          workspaceId={workspaceId}
          side={z.side}
          className={z.className}
        />
      ))}
    </div>
  );
}

function TileEdgeZone({
  workspaceId,
  side,
  className,
}: {
  workspaceId: string;
  side: TileDropSide;
  className: string;
}) {
  const { setNodeRef, isOver } = useDroppable({
    id: useCliDndId(tileZoneId(workspaceId, side)),
    disabled: useCliDragStale(),
  });
  return (
    <div
      ref={setNodeRef}
      data-drop-side={side}
      data-drop-over={isOver ? "true" : "false"}
      className={`absolute ${className} ${
        isOver
          ? "bg-[var(--cs-accent,#388bfd)]/30 outline outline-2 outline-[var(--cs-accent,#388bfd)]"
          : ""
      }`}
    />
  );
}

interface WorkspaceTerminalBodyProps {
  workspace: Workspace;
  teamKey: string;
  /** The CLI column is on screen. */
  shown: boolean;
  /** The team's grid is hidden behind another team's: keep the panes mounted. */
  teamHidden: boolean;
  isFocused: boolean;
  terminalFontPx: number;
  themeConfig: TerminalThemeConfig;
}

// ---------------------------------------------------------------------------
// WorkspaceInitOverlay (SLW-CO-5 → SLW-CO-6) — inline workspace-setup progress
// shown over the terminal while `status === "creating"`, replacing the retired
// detached "Setting up…" toast. Structurally mirrors the terminal's own init
// overlay (`shell-terminal-panel.tsx`): an `absolute inset-0 z-10` sibling with
// the same theme background + `DotPulse` spinner, so the two overlays hand off
// seamlessly (workspace-init → terminal-init) with no visual jump.
//
// SLW-CO-6 (AC-3): generalized from story-only to ANY `status==="creating"`
// workspace. The heading is per-kind; Cancel renders only when the create step
// is cancellable (`onCancel` provided) — a story's `setupWorktree` is abortable,
// an epic's fast dir-ensure is not.
// ---------------------------------------------------------------------------

/** Per-kind overlay heading (AC-3). Falls back to a generic heading for any
 *  future kind that materializes without its own copy. */
const INIT_HEADING_BY_KIND: Partial<Record<Workspace["kind"], string>> = {
  story: "Setting up worktree…",
  epic: "Setting up epic workspace…",
};
const DEFAULT_INIT_HEADING = "Setting up workspace…";

export interface WorkspaceInitOverlayProps {
  /** Workspace kind — drives the heading (AC-3). */
  kind: Workspace["kind"];
  /** A plain sub-label (the pre-record "Checking worktree…", an epic's
   *  assembly count) when there is no typed setup state. */
  setupLabel?: string;
  /** SW-XS-10: the typed setup line (Step 1's phase and counts). */
  setupState?: WorkspaceSetupState;
  /** Identity shown under the heading (story title/id, epic doc, …). */
  identity?: string;
  themeConfig: TerminalThemeConfig;
  /** AC-8 / AC-3: cancel the in-flight create + tear down the just-mounted
   *  workspace. Rendered ONLY when provided — a cancellable create (story) shows
   *  Cancel; a non-cancellable one (epic dir-ensure) omits it. */
  onCancel?: () => void;
  /** SW-WU-7 (AC-9): coarse phase text announced via a polite `sr-only` live
   *  region — set at PHASE granularity (not per progress tick) so screen readers
   *  hear the phase change once, not a chatty counter. */
  announce?: string;
}

/** SW-SO-1: exported so the pre-record "opening" overlay (team-workspace-view)
 *  reuses the EXACT same loader as the in-tile one — one component, one set of
 *  labels/affordances, no fork (AC-8). */
export function WorkspaceInitOverlay({
  kind,
  setupLabel,
  setupState,
  identity,
  themeConfig,
  onCancel,
  announce,
}: WorkspaceInitOverlayProps) {
  const shown = useDisplayedLine({ base: setupState });
  const view = describeSetup(shown);
  return (
    <div
      data-surface="terminal"
      className="absolute inset-0 flex flex-col items-center justify-center z-10 gap-3 px-6 text-center"
      style={{ backgroundColor: themeConfig.theme.background }}
      data-testid="workspace-init-overlay"
    >
      <DotPulse size={40} speed={1.3} color="var(--cs-status-busy)" />
      <p className="text-sm font-semibold text-[var(--foreground)]">
        {INIT_HEADING_BY_KIND[kind] ?? DEFAULT_INIT_HEADING}
      </p>
      {identity && <p className="text-xs text-muted-foreground max-w-full truncate">{identity}</p>}
      {view ? (
        // SW-XS-10: the setup line — phase left, count right, `--accent` ring;
        // it carries its own once-per-phase status announcement.
        <SetupLineView view={view} variant="overlay" />
      ) : (
        <>
          <p className="text-xs text-muted-foreground">{setupLabel ?? "Preparing…"}</p>
          {/* SW-WU-7 (AC-9): polite, phase-granular announcement. Always present so the
              text SWAP is what the screen reader picks up; empty renders nothing. */}
          <div aria-live="polite" className="sr-only" data-testid="workspace-init-announce">
            {announce ?? ""}
          </div>
        </>
      )}
      {onCancel && (
        <button
          onClick={onCancel}
          className="mt-1 px-3 py-1.5 text-xs font-medium rounded-md border border-[var(--border)] text-[var(--muted-foreground)] transition-colors enabled:hover:bg-[var(--accent)] disabled:cursor-not-allowed disabled:opacity-50"
        >
          Cancel
        </button>
      )}
    </div>
  );
}

/** SW-EW-46: how ONE rendered terminal body presents.
 *  - **grid cell** (`isCell`): fills its region of the tab grid. Its outline is the
 *    pane-level focus line (`.cs-focus-pane`), painted from real DOM focus —
 *    never from which tab is active.
 *  - **tab body** (not a cell): the active tab of a workspace on Tabs, an
 *    absolute overlay filling the pane. */
function paneBodyView(isCell: boolean): string {
  return isCell
    ? "cs-focus-pane relative flex h-full w-full min-h-0 min-w-0 flex-col overflow-hidden"
    : // A flex column like a cell: caption on top, the body (terminal and its docked inbox)
      // filling exactly the height left over. As a plain block, the caption plus an `h-full`
      // body overflowed the tile by one caption height — the room a focused terminal
      // scrolled the tile up by, and where the inbox composer would sit below the tile's
      // bottom edge. `overflow-clip` so nothing can scroll it programmatically either.
      "absolute inset-0 flex min-h-0 flex-col overflow-clip";
}

/** A grid cell's readable floor along each axis: a row's cells keep a width,
 *  a column's rows keep a height. */
const CELL_MIN_PX = { row: MIN_CELL_WIDTH_PX, column: MIN_CELL_HEIGHT_PX } as const;

interface TabGridViewProps {
  teamKey: string;
  workspaceId: string;
  /** The cells, in strip order. */
  cellIds: string[];
  /** How many columns / rows fit above the readable floor (Infinity unmeasured). */
  maxCols: number;
  maxRows: number;
  renderCell: (tabId: string) => ReactNode;
}

/** A workspace's tab grid: its cells on the same mosaic as the workspace view, laid
 *  out by the workspace's preset and resized by dragging the dividers. */
function TabGridView({
  teamKey,
  workspaceId,
  cellIds,
  maxCols,
  maxRows,
  renderCell,
}: TabGridViewProps) {
  const preset = useWorkspaceTabGridPreset(teamKey, workspaceId);
  const saved = useTabGridLayout(teamKey, workspaceId);
  const root = useMemo(
    () => reconcilePresetTree(saved, preset, cellIds, maxCols, maxRows),
    [saved, preset, cellIds, maxCols, maxRows],
  );
  // More cells than fit above the floor: no layout can hold it, so the cells
  // share the pane without one rather than ask for an impossible minimum.
  const fits = cellIds.length <= maxCols * maxRows;
  return (
    <MosaicView
      node={root}
      renderLeaf={renderCell}
      minSizePx={fits ? CELL_MIN_PX : undefined}
      onSizesCommit={(path, sizes) => {
        if (root) {
          getViewerState().setTabGridSizes(teamKey, workspaceId, preset, root, path, sizes);
        }
      }}
    />
  );
}

function WorkspaceTerminalBody({
  workspace: ws,
  teamKey,
  shown,
  teamHidden,
  isFocused,
  terminalFontPx,
  themeConfig,
}: WorkspaceTerminalBodyProps) {
  const { tabs, activeTabId } = useWorkspaceTabs(teamKey, ws.id);
  const [selection, setSelection] = useState<{ current: string | undefined; previous?: string }>({
    current: activeTabId,
  });
  // Keep the previous pane in this commit's set, including a distant selection.
  if (selection.current !== activeTabId) {
    setSelection({ current: activeTabId, previous: selection.current });
  }
  const newTerminalKeystroke = formatKeystrokeDisplay(
    useEffectiveShortcut("viewer.new-terminal-tab"),
  );
  const layoutKnown = useTerminalLayoutHydrated();
  const hostsUnreachable = useTerminalHostsUnreachable();
  const { phase: serverPhase } = useTerminalServer();
  // The app lost This Mac's terminal server, which still runs (asked, so a
  // stop, restart or update on purpose never reads as reconnecting).
  const lostServer = useLocalServerUnreachable(
    useLocalTerminalServerReconnecting() && serverPhase.kind !== "restarting",
  );
  const serverUnreachable = lostServer === "unreachable";
  // Said over the tabs only where this pane runs terminals on This Mac.
  const reconnecting = serverUnreachable && tabs.some((tab) => !tab.exit && !tab.origin);
  const heldOpens = useHeldTerminalOpens(teamKey, ws.id);
  // Grid cells all stay mounted. Tabs retain a bounded set of hidden panes;
  // displays outside that set remain parked in the terminal manager.
  const panedIds = useWorkspacePanedTabIds(teamKey, ws.id);
  const showGrid = useShouldRenderTabGrid(teamKey, ws.id);
  const canGrid = useIsExpandedWorkspace(teamKey, ws.id);
  // The grid's cells: the paned tabs that still exist, in strip order.
  const cellIds = useMemo(() => {
    const paned = new Set(panedIds);
    return tabs.filter((t) => paned.has(t.id)).map((t) => t.id);
  }, [tabs, panedIds]);
  // SW-EW-46: measure the pane so the grid's shape can be FLOOR-CLAMPED — the
  // column/row count is capped to keep every cell above the readable minimum (a
  // 9-col cell corrupts a reflowing shell prompt). `absolute inset-0` fills the
  // pane, so this is the pane size; 0 (pre-measure) → Infinity caps → the raw
  // preset shape until the RO reports.
  const gridWrapRef = useRef<HTMLDivElement>(null);
  const [gridWrapSize, setGridWrapSize] = useState({ w: 0, h: 0 });
  // Only an expandable workspace can ever show a tab grid, so only it needs the measured cap.
  // Gating on `canGrid` keeps every tab-mode tile in a mosaic from running an idle
  // observer that re-renders its terminal body on each pane resize.
  useEffect(() => {
    if (!canGrid) return;
    const el = gridWrapRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const r = entries[0]?.contentRect;
      if (r) setGridWrapSize({ w: r.width, h: r.height });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [canGrid]);
  const maxCols =
    gridWrapSize.w > 0 ? Math.max(1, Math.floor(gridWrapSize.w / MIN_CELL_WIDTH_PX)) : Infinity;
  const maxRows =
    gridWrapSize.h > 0 ? Math.max(1, Math.floor(gridWrapSize.h / MIN_CELL_HEIGHT_PX)) : Infinity;
  // The cell the keyboard moves to when a cell is closed from its caption: the
  // next cell, else the one before.
  const cellNeighbour = useMemo(
    () => new Map(cellIds.map((id, i) => [id, cellIds[i + 1] ?? cellIds[i - 1]])),
    [cellIds],
  );

  // SLW-CO-7/CO-8: the overlay owns the WHOLE sync. The story-vs-epic derivation
  // + the one-shot clear-on-settle + the materialize-start watchdog all live in
  // one focused hook (`useWorkspaceOverlayState`) so the body stays a thin render.
  const overlay = useWorkspaceOverlayState(ws, teamKey);
  const operationAnnouncement = useOperationAnnouncement(ws);
  // SW-LN-29 (M-28): a command held for context that had to run without it
  // (hold timeout). Cleared once the context lands, so the chip then says ready.
  const [contextHoldTimedOut, setContextHoldTimedOut] = useState(false);
  const [contextHeld, setContextHeld] = useState(false);
  const onContextHoldTimeout = useCallback(() => setContextHoldTimedOut(true), []);
  const onContextHold = useCallback(() => setContextHeld(true), []);
  const onContextHoldDropped = useCallback(() => setContextHeld(false), []);
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- the held command runs when landing ends
    if (!overlay.contextLanding) setContextHeld(false);
    // Only a real landing clears the "ran without context" warning.
    if (overlay.contextLanded) setContextHoldTimedOut(false);
  }, [overlay.contextLanding, overlay.contextLanded]);
  // AC-4: a settled-but-usable epic with some failed repos shows a dismissible
  // in-pane flag over the (usable) terminal until the user acknowledges it.
  const [partialFailureDismissed, setPartialFailureDismissed] = useState(false);

  const paneFocusIndicator = usePaneFocusIndicator(focusTileKey(teamKey, ws.id));
  // SW-TP CAP-12: per-tile focus signal for the per-tab caption's bottom
  // border. Non-undefined only when the tile is focused; the active tab's
  // caption threads this (see renderPane below), non-active cells stay
  // neutral so the one-blue-line-per-level rule still holds.
  const tileFocusIndicator = useTileFocusIndicator(focusTileKey(teamKey, ws.id));
  const selectCell = (tabId: string, explicit = false) => {
    const store = getViewerState();
    // Passive autofocus on the displayed fallback is not a local choice while
    // the saved remote tab reconnects. A press or a different cell is navigation.
    if (!explicit && selectActiveTerminalTab(store, teamKey, ws.id) === tabId) return;
    if (explicit) beginContextNavigation();
    store.setActiveTerminalTab(teamKey, tabId, ws.id);
  };
  // A tab's pane is keyed by its lives: a new process started in the tab (the
  // ended bar's Restart / Relaunch) gets a new pane attached to it.
  const lives = useTerminalLives();

  // Hidden tiles retain their existing active pane, without extra warm neighbours, unless the
  // whole grid is a warm team's, hidden for another team: nothing there is torn down.
  const mountedIds =
    shown || teamHidden
      ? warmTerminalTabIds(tabs, activeTabId, selection.previous, true)
      : tabs.filter((tab) => tab.id === activeTabId).map((tab) => tab.id);

  // One tab's body: a grid cell (its caption above its terminal) or the active
  // tab on Tabs. `data-pane-id` + click-to-focus are the cell-only extras (D6: a
  // cell is a directionally-navigable pane keyed by its tab id).
  const renderPane = (tabIndex: number, isCell: boolean) => {
    const tab = tabs[tabIndex];
    const isActiveTabInWs = tab.id === activeTabId;
    const isVisible = shown && (isCell || isActiveTabInWs);
    const paneFocus = isCell ? paneFocusIndicator(tab.id) : undefined;
    const autoFocus = isFocused && isActiveTabInWs;
    return (
      <div
        key={tab.id}
        className={paneBodyView(isCell)}
        style={isVisible ? undefined : { display: "none" }}
        inert={!isVisible}
        aria-hidden={!isVisible || undefined}
        data-pane-id={isCell ? tab.id : undefined}
        data-testid={isCell ? `terminal-grid-cell-${tab.id}` : undefined}
        {...{ [FOCUS_PANE_ATTR]: tab.id }}
        data-pane-active={isActiveTabInWs ? "" : undefined}
        data-focus-indicator={paneFocus}
        // A cell selects its tab when pressed or when focus reaches it
        // any other way (Tab, ⌘⌥ arrows), so the store follows focus.
        onPointerDownCapture={isCell ? () => selectCell(tab.id, true) : undefined}
        onFocusCapture={isCell ? () => selectCell(tab.id) : undefined}
      >
        {/* SW-UX-CLI-10 / SW-TP CAP-6: the per-tab toolbar strip. Previously
            grid-cell-only (SW-UX-CLI-10); now mounted for EVERY visible tab
            (grid-mode: one per cell; tabs-mode: one for the active tab) so the
            toolbar replaces the retired standalone TabPrimitivesToolbar. */}
        {isVisible && (
          <TerminalGridCaption
            tab={tab}
            isActive={isActiveTabInWs}
            background={themeConfig.chromeBg}
            foreground={themeConfig.theme.foreground ?? "inherit"}
            onFocusCell={() => selectCell(tab.id, true)}
            onZoomCell={() => zoomTerminalCell(teamKey, ws.id, tab.id)}
            onClose={() => closeTab(teamKey, ws.id, tab.id)}
            neighbourTabId={isCell ? cellNeighbour.get(tab.id) : undefined}
            workspace={ws}
            teamKey={teamKey}
            // SW-TP CAP-12: thread the owning tile's focus indicator only for the
            // active tab's caption. Non-active cell captions in a split stay neutral
            // so the one-line-per-tile rule holds.
            tileFocusIndicator={isActiveTabInWs ? tileFocusIndicator : undefined}
          />
        )}
        {/* `isCell`-only behavior (cell-focus marker, keyboard close targeting
            via `data-pane-id`) remains gated above; the caption itself is now
            universal. Hidden tiles keep their existing rail; warm neighbours do not. */}
        <InboxSplit tab={tab} teamKey={teamKey} visible={isVisible || !shown}>
          {tab.origin ? (
            <RemoteTerminalPane
              tab={tab}
              teamKey={teamKey}
              workspaceId={ws.id}
              workspaceKey={workspaceKey(teamKey, ws)}
              visible={isVisible}
              autoFocus={autoFocus}
              attachGeneration={currentActivationGeneration(teamKey)}
              fontSizePx={terminalFontPx}
              themeConfig={themeConfig}
            />
          ) : (
            <ShellTerminalPanel
              key={lives[tab.id] ?? 0}
              life={lives[tab.id] ?? 0}
              sessionId={tab.sessionId}
              exit={tab.exit}
              workspaceId={ws.id}
              harnessPaneKey={harnessPaneKey(ws.id, tabIndex, tab.id)}
              teamKey={teamKey}
              visible={isVisible}
              attachGeneration={currentActivationGeneration(teamKey)}
              autoFocus={autoFocus}
              fontSizePx={terminalFontPx}
              themeConfig={themeConfig}
              contextLanding={overlay.contextLanding}
              onContextHold={onContextHold}
              onContextHoldDropped={onContextHoldDropped}
              onContextHoldTimeout={onContextHoldTimeout}
            />
          )}
        </InboxSplit>
      </div>
    );
  };

  return (
    <div data-surface="terminal" className="h-full w-full relative">
      <NewTerminalChooser teamKey={teamKey} workspaceId={ws.id} />
      {/* SW-XS-10 (§8.6): the workspace's one always-mounted status region. */}
      <WorkspaceAnnouncer ws={ws} operation={operationAnnouncement} />
      <PendingRemoteOpens teamKey={teamKey} workspaceId={ws.id} />
      <RemoteWorkspaceOffline teamKey={teamKey} ws={ws} />
      {/* SLW-CO-7 (AC-1/AC-6): the overlay owns the whole sync. It covers the
          terminal through create AND materialize (`overlayVisible`), then hands
          off IN-PANE to the shared `WorkspaceErrorPlaceholder` if the
          materialization wave FAILS (`overlayFailed`) — reusing the CO-5 error
          surface rather than overloading `ws.status`. Cancel shows only during a
          cancellable CREATE (story `setupCancellable`), never during the
          non-cancellable background materialize. Recovery is close + reopen. */}
      {overlay.failed ? (
        <WorkspaceMaterializeError
          workspaceId={ws.id}
          teamKey={teamKey}
          // SLW-CO-8: an epic's total-failure reason is the assemble error.
          reason={overlay.failureReason}
          themeConfig={themeConfig}
        />
      ) : overlay.visible ? (
        <WorkspaceInitOverlay
          kind={ws.kind}
          setupLabel={overlay.subLabel}
          setupState={overlay.setupState}
          identity={ws.storyTitle ?? ws.storyId ?? ws.epicDocPath ?? workspaceDisplayLabel(ws)}
          themeConfig={themeConfig}
          onCancel={
            ws.status === "creating" && ws.setupCancellable
              ? () => getViewerState().closeWorkspace(teamKey, ws.id)
              : undefined
          }
        />
      ) : null}
      {/* SW-LN-29 (M-28): says when the team context has landed in this worktree;
          sits above the init overlay so it is visible while the context lands. */}
      <ContextLandingChip
        landing={overlay.contextLanding}
        landed={overlay.contextLanded}
        failed={overlay.contextFailed}
        held={contextHeld}
        timedOut={contextHoldTimedOut}
        surface={ws.cwd}
      />
      {/* SLW-CO-8 (AC-4): a settled-but-usable epic with partial failures — the
          overlay has cleared (epic is usable), so flag the failed repos in-pane
          WITHOUT blocking the terminal. Dismissible; the chip carries the same
          echo for when the epic is backgrounded. */}
      {overlay.partialFailure && !overlay.visible && !partialFailureDismissed && (
        <div
          className="absolute top-0 left-0 right-0 z-20 flex items-start gap-2 px-3 py-2 text-xs bg-[var(--popover)] border-b border-[var(--border)] text-[var(--foreground)]"
          data-testid="epic-partial-failure"
        >
          <AlertCircle className="h-3.5 w-3.5 shrink-0 mt-0.5 text-[var(--cs-warning-text)]" />
          <span className="flex-1">{overlay.partialFailureLabel}</span>
          <button
            onClick={() => setPartialFailureDismissed(true)}
            className="shrink-0 text-[var(--muted-foreground)] hover:text-[var(--foreground)] transition-colors"
            aria-label="Dismiss"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}
      {(heldOpens > 0 || reconnecting) && tabs.length > 0 && (
        // Said over a pane showing tabs (an empty pane says it in its own flow,
        // below). SW-RT-6d: new terminals the incompatible terminal server
        // refused wait for its restart. And while the app reconnects to This
        // Mac's terminal server, its terminals keep running: the tabs stay.
        <div className="absolute top-0 left-0 right-0 z-20 flex flex-col items-center gap-2 p-2">
          {heldOpens > 0 && (
            <div data-testid="workspace-terminal-held">
              <TerminalHeldNotice count={heldOpens} />
            </div>
          )}
          {reconnecting && <TerminalReconnectingBanner />}
        </div>
      )}
      {tabs.length === 0 &&
        (serverUnreachable || (!layoutKnown && hostsUnreachable)) &&
        !overlay.visible && (
          // The hosts have not answered yet: "not known", never a false empty.
          <div
            className="h-full flex items-center justify-center p-4"
            style={{ backgroundColor: themeConfig.theme.background }}
          >
            <TerminalHostsReconnecting />
          </div>
        )}
      {tabs.length === 0 &&
        layoutKnown &&
        // Not "No terminals running" while the lost server is still being asked about.
        (lostServer === "connected" || lostServer === "notRunning") &&
        !overlay.visible &&
        !overlay.failed && (
          // The workspace's normal empty state: nothing auto-starts here (a
          // restored workspace whose terminals ended opens empty) — the user starts
          // one.
          <div
            data-testid="workspace-terminal-empty"
            className="h-full flex flex-col items-center justify-center gap-3 p-4 text-center"
            style={{ backgroundColor: themeConfig.theme.background }}
          >
            {heldOpens > 0 ? (
              <div data-testid="workspace-terminal-held" className="max-w-md">
                <TerminalHeldNotice count={heldOpens} />
              </div>
            ) : (
              <TerminalServerNotice />
            )}
            <p className="text-sm text-muted-foreground">No terminals running</p>
            <button
              type="button"
              data-testid="workspace-terminal-empty-new"
              onClick={() => {
                newTerminalTab(teamKey, ws.id);
              }}
              className="inline-flex items-center gap-2 rounded bg-[#1f6feb] px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-[#388bfd] focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[#58a6ff]"
            >
              New terminal
              {newTerminalKeystroke && (
                <kbd
                  data-testid="workspace-terminal-empty-new-shortcut"
                  className="rounded border border-white/30 bg-white/10 px-1 font-mono text-[10px]"
                >
                  {newTerminalKeystroke}
                </kbd>
              )}
            </button>
          </div>
        )}
      {/* Keyed siblings retain their pane and slot when selection moves. */}
      <div ref={gridWrapRef} className="absolute inset-0">
        {showGrid ? (
          <TabGridView
            teamKey={teamKey}
            workspaceId={ws.id}
            cellIds={cellIds}
            maxCols={maxCols}
            maxRows={maxRows}
            renderCell={(tabId) => {
              const index = tabs.findIndex((t) => t.id === tabId);
              return index === -1 ? null : renderPane(index, true);
            }}
          />
        ) : (
          mountedIds.map((id) =>
            renderPane(
              tabs.findIndex((tab) => tab.id === id),
              false,
            ),
          )
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// useTileTabActions — tab bundle scoped to ONE tile's workspace
// ---------------------------------------------------------------------------

interface TileTabActions {
  onSelectTab: (tabId: string, opts?: SelectTabOptions) => void;
  /** The default close (`×`, middle-click, Delete): Hide when running, End
   *  when idle. Resolves false when nothing closed (End's ask cancelled). */
  onCloseTab: (tabId: string) => Promise<boolean>;
  onHideTab: (tabId: string) => void;
  onEndTab: (tabId: string) => void;
  /** Restart a tab whose process ended: a new process in the same tab (the
   *  ended bar's Restart). */
  onRestartTab: (tabId: string) => void;
  onEndOtherTabs: (tabId: string) => void;
  onEndAllTabs: () => void;
  onNewShell: () => void;
  /** The machine chooser: a new terminal on another machine, or an agent there. */
  onNewShellOn: () => void;
  /** Split: a new terminal beside `tabId`, side by side, focused. */
  onSplitTab: (tabId: string) => void;
  onReorderTab: (fromIdx: number, toIdx: number) => void;
  onRenameTab: (tabId: string, name: string) => void;
  /** SW-EW-46 (D2): the tab-grid dial. Pane a single tab out as a live grid cell,
   *  return a cell to the bar, or show all tabs at once / one tab at a time. */
  onPaneOutTab: (tabId: string) => void;
  onDemotePane: (tabId: string) => void;
  onShowAllTabs: () => void;
  onShowOneTab: () => void;
}

/**
 * SW-EW-3 (D-3/D-4): per-tile tab actions. Each handler `focusTile()` first so
 * the focused-tile border follows the click (the EW-2 active-terminal invariant
 * generalized), then mutates with this tile's explicit `workspaceId` so the
 * action targets the right workspace with no reliance on focus timing — and the
 * tab state is the existing per-workspace slice (no per-pane state).
 */
export function useTileTabActions(
  teamKey: string,
  workspaceId: string,
  focusTile: () => void,
): TileTabActions {
  return useMemo<TileTabActions>(() => {
    return {
      // A single pointer click on a tab also moves keyboard focus into it: in a
      // grid the tab is an already-visible cell, so no reveal would move focus.
      // Keyboard activation keeps focus on the tab (the caller omits the flag),
      // and a rename opened meanwhile keeps its input.
      onSelectTab: (tabId, opts) => {
        beginContextNavigation();
        focusTile();
        getViewerState().setActiveTerminalTab(teamKey, tabId, workspaceId);
        if (opts?.focusTerminal) {
          activateAndFocus({ teamKey, workspaceId, yieldToEditing: true });
        }
      },
      // The close verbs all go through `lib/terminal-close` (the one decision
      // point); each targets this tile's workspace explicitly.
      onCloseTab: (tabId) => {
        focusTile();
        return closeTab(teamKey, workspaceId, tabId);
      },
      onHideTab: (tabId) => {
        focusTile();
        hideTab(teamKey, workspaceId, tabId);
      },
      onEndTab: (tabId) => {
        focusTile();
        void endTabs(teamKey, workspaceId, { tabId });
      },
      onRestartTab: (tabId) => {
        focusTile();
        restartTab(teamKey, workspaceId, tabId);
      },
      onEndOtherTabs: (tabId) => {
        focusTile();
        void endTabs(teamKey, workspaceId, { except: tabId });
      },
      onEndAllTabs: () => {
        focusTile();
        void endTabs(teamKey, workspaceId, "all");
      },
      onNewShell: () => {
        focusTile();
        newTerminalTab(teamKey, workspaceId);
      },
      onNewShellOn: () => {
        focusTile();
        requestNewWorkspaceTerminal(teamKey, workspaceId);
      },
      onSplitTab: (tabId) => {
        focusTile();
        splitTerminal(teamKey, { workspaceId, tabId });
      },
      onReorderTab: (fromIdx, toIdx) => {
        beginContextNavigation();
        focusTile();
        getViewerState().reorderTerminalTab(teamKey, fromIdx, toIdx, workspaceId);
      },
      onRenameTab: (tabId, name) => {
        focusTile();
        getViewerState().renameTerminalTab(teamKey, tabId, name, workspaceId);
      },
      // SW-EW-46 (D2): tab-grid dial — thin wrappers over the store mutators, each
      // focusing this tile first (like the tab actions above).
      onPaneOutTab: (tabId) => {
        beginContextNavigation();
        focusTile();
        getViewerState().paneOutTerminalTab(teamKey, workspaceId, tabId);
      },
      onDemotePane: (tabId) => {
        beginContextNavigation();
        focusTile();
        getViewerState().demoteTerminalPane(teamKey, workspaceId, tabId);
      },
      // Showing side by side after a cell's zoom brings back the zoom's cells.
      onShowAllTabs: () => {
        focusTile();
        showAllTabs(teamKey, workspaceId);
      },
      onShowOneTab: () => {
        focusTile();
        showOneTab(teamKey, workspaceId);
      },
    };
  }, [teamKey, workspaceId, focusTile]);
}
