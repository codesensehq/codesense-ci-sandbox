// SPDX-License-Identifier: MIT
import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useState,
  type ComponentProps,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { useTabAgentLook } from "@/features/viewer/hooks/use-tab-agent-look";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { isFleetAgent } from "@/features/viewer/lib/agent-rows";
import { useAgentStop } from "./agent-stop-dialog";
import { useTabTimerAgent } from "@/features/viewer/hooks/use-tab-timer-agent";
import { RemindPopover } from "./remind-popover";
import { WorkspaceMachineMark } from "@/features/machines/components/machine-location-badge";
import {
  Maximize,
  Minimize,
  Minus,
  X,
  PanelTopOpen,
  ChevronDown,
  FolderKanban,
  FileDiff,
  Code,
  MoreVertical,
  LayoutGrid,
  Check,
  Columns2,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type {
  TerminalTab,
  Workspace,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { cn } from "@/lib/utils";
import { getViewerState } from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import {
  useStoryHudShown,
  useSourceDiffShown,
  useWorktreeEditorOpen,
  useEditorChoice,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import { getKindCapabilities, showsStoryStatus } from "@/shared/lib/workspace-kind-capabilities";
import type { PipelineStatus } from "@/shared/lib/pipeline-graph/types";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { ariaKeyshortcuts, formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { InfoTip } from "@/components/ui/tooltip";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuCheckboxItem,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuShortcut,
} from "@/components/ui/dropdown-menu";
import { canOpenWorkspaceDoc, openWorkspaceDoc } from "../lib/story-doc-gesture";
import { editorLaunchLabel } from "../lib/editor-launch-label";
import { useDetectedEditors } from "@/shared/hooks/use-detected-editors";
import { openInEditorFromCwd } from "@/shared/lib/open-in-editor";
import { useWorkspaceLabel } from "../hooks/use-workspace-label";
import { RepoGlyph } from "@/shared/components/repo-glyph";
import { StatusMark } from "@/shared/components/status-mark";
import { ScopedStatusMark, SessionStatusMark, useSessionEnd } from "./session-status-mark";
import { StatusSelectorPopover } from "@/shared/components/status-selector-popover";
import { isQuietMark } from "@/shared/lib/status-mark";
import { StageRail } from "@/shared/components/stage-rail";
import { TerminalTabBar, type SelectTabOptions } from "./terminal-tab-bar";
import { useWorkspaceTeamHue } from "../hooks/use-workspace-crews";
import { useActiveTeamKey } from "@/shared/lib/team-key";
import { focusWorkspaceTerminal } from "../lib/focus-cascade";
import type { FocusIndicator } from "../lib/focus-indicator";
import {
  requestTabRename,
  useTabRenameRequestStore,
  type TabRenameRequest,
} from "../lib/terminal-tab-keys";
import { TabContextMenu, type KeyHint } from "./tab-context-menu";
import { MiddleTruncate } from "./middle-truncate";
import { type DotScope, useScopeMark } from "../hooks/use-active-workspace-terminal-tabs";
import {
  CHROME_CLUSTER_PAD_W,
  CHROME_CONTROL_W,
  CHROME_HIDDEN_HINT_COMPACT_W,
  CHROME_HIDDEN_HINT_W,
  CHROME_NEW_TAB_W,
  CHROME_OVERFLOW_W,
  CHROME_ROLLUP_W,
  MAX_VISIBLE_TABS,
  MAX_VISIBLE_TABS_ZOOMED,
  TAB_PILL_MIN_W,
  fitTileChrome,
  overflowLabel,
} from "../lib/tile-chrome-fit";
import { closeTooltip, useCloseVerb } from "../lib/terminal-close";
import { setSourceDiffReturnFocus } from "./source-diff-return-focus";

// ---------------------------------------------------------------------------
// SW-EW-3 — TerminalTileChrome: the per-tile identity + tab bar (AC-5/6/7).
//
// One thin bar per split tile: `[status] STORY-ID │ <tabs> │ ⌄+N ×`.
// Reuses the live identity vocabulary (the shared status mark, storyDisplayLabel,
// TerminalTabBar) rather than inventing any. Only renders in split mode (>=2
// tiles); 1-up keeps today's chrome.
// ---------------------------------------------------------------------------

/** Track an element's rendered width (CSS px, rounded). `null` until the first
 *  non-zero measurement, so an unmeasured chrome keeps the count-only layout.
 *  The first measure runs before paint, so the fitted layout never flashes. */
function useMeasuredWidth(): [(el: HTMLElement | null) => void, number | null] {
  const [el, setEl] = useState<HTMLElement | null>(null);
  const [width, setWidth] = useState<number | null>(null);
  useLayoutEffect(() => {
    if (!el) return;
    const update = (w: number) => {
      if (w > 0) setWidth(Math.round(w));
    };
    update(el.getBoundingClientRect().width);
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver((entries) => {
      update(entries[0]?.contentRect.width ?? 0);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [el]);
  return [setEl, width];
}

/** Toolbar controls follow their terminal surface at rest, on hover and on focus. */
const CHROME_ICON_BUTTON =
  "flex flex-shrink-0 h-6 items-center justify-center rounded text-muted-foreground transition-colors hover:bg-foreground/5 hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring";

/** Whether the ⋮ menu has view toggles of its own for this workspace kind.
 *  SW-VE-12: `sourceCapable` folds in the code-editor on every folder-rooted
 *  kind (TRUE for every live kind). The kebab therefore renders on every
 *  tile, independent of the diff gate — `sourceCapable` already dominates
 *  `diffCapable` so no need to OR it in. */
function hasViewOptions(kind: Workspace["kind"]): boolean {
  const caps = getKindCapabilities(kind);
  return Boolean(caps.hudToggle || caps.sourceCapable);
}

/** A control folded into the ⋮ menu when the tile is too narrow to show it. */
interface FoldedControl {
  key: string;
  icon: LucideIcon;
  label: string;
  onSelect: () => void;
}

export interface TerminalTileChromeProps {
  workspace: Workspace;
  /** The team the workspace belongs to. The CLI can show a team other than the
   *  active one, so the tile names it; without it, the active team. */
  teamKey?: string;
  /** The store's active tile (`activeWorkspace`): a brighter, bolder label.
   *  It never paints the blue focus line — see `focusIndicator`. */
  isFocused: boolean;
  /** The blue focus line under the chrome, derived from real DOM focus
   *  (`lib/focus-indicator`): on, dim once focus has left, or none. */
  focusIndicator?: FocusIndicator;
  /** SW-EW-34 v2 (D8): drop the `[status] WORKSPACE-ID` identity block (the
   *  top console switcher owns identity on a lone tile). Set only for a 1-up
   *  console tile; split tiles keep the id to disambiguate. */
  hideIdentity?: boolean;
  /** AC-5: 2-char source-repo glyph (multi-repo only). */
  glyph?: string;
  glyphColor?: string;
  tabs: TerminalTab[];
  activeTabId: string | undefined;
  /** The terminal theme's chrome colour — the per-tab caption below this bar.
   *  The active tab takes it so tab and caption read as one piece. */
  chromeBg?: string;
  /** The terminal theme's background; the tab bar is this, darkened by the
   *  `--cs-terminal-tabbar-*` tokens. */
  terminalBg?: string;
  /** Focus-then-act bundle — each handler first focuses this tile's workspace. */
  onSelectTab: (tabId: string, opts?: SelectTabOptions) => void;
  /** The tab `×` / middle-click — Hide when running, End otherwise. */
  onCloseTab: (tabId: string) => void | Promise<boolean>;
  /** The tab menu's two verbs (see `lib/terminal-close`). */
  onHideTab: (tabId: string) => void;
  onEndTab: (tabId: string) => void;
  /** Restart a tab whose process ended — offered in its menu only then. */
  onRestartTab?: (tabId: string) => void;
  onEndOtherTabs: (tabId: string) => void;
  onEndAllTabs: () => void;
  onNewShell: () => void;
  /** The machine chooser. Absent → no "New terminal on…" (bare chrome / tests). */
  onNewShellOn?: () => void;
  /** Split: a new terminal beside the tab, side by side and focused (⌘D). Absent
   *  → no Split item (bare chrome / tests). */
  onSplitTab?: (tabId: string) => void;
  onReorderTab: (fromIdx: number, toIdx: number) => void;
  onRenameTab: (tabId: string, name: string) => void;
  /** `–` Hide the tile into the tray, every terminal still running. Available
   *  at every N (including N=1). Absent → no `–` (bare chrome / tests). */
  onHideTile?: () => void;
  /** `×` the default close for the whole tile — Hide when a command runs in the
   *  foreground, End otherwise. Absent → no `×` (bare chrome / tests). */
  onCloseTile?: () => void;
  /** AC-5: focus this tile on any chrome press — selects it and, when the
   *  press is not on a control, moves keyboard focus into its terminal. */
  onFocusTile: (event: ReactMouseEvent) => void;
  /** SW-EW-12 AC-11: double-click the story-id → open the story doc (reuses
   *  `openFile` via the team view). Restores
   *  the behavior the retired flat WorkspaceBar pill carried. Story tiles only —
   *  no-op without a `storyFilePath`. */
  onOpenStoryDoc?: (storyFilePath: string) => void;
  /** SW-EW-18 AC-6: set the story's lifecycle status from the identity dropdown
   *  (the `WorkspacePill` pattern). Story tiles only; absent → the id renders
   *  with no dropdown. */
  onStoryStatusChange?: (newStatus: PipelineStatus) => void;
  /** SW-EW-4 AC-1: this tile is zoomed (full-region). Flips the toggle to
   *  "restore" and reveals the "+N hidden" hint. */
  isZoomed?: boolean;
  /** SW-EW-4 AC-1: toggle zoom for this tile. Absent → no toggle rendered
   *  (the toggle only exists at N≥2, where the chrome itself shows). */
  onToggleZoom?: () => void;
  /** SW-EW-4 AC-1 (locked rule 6): sibling tiles hidden behind the zoom — the
   *  "+N hidden" hint. Only meaningful while `isZoomed`. */
  hiddenTileCount?: number;
  /** SW-EW-4 AC-4: the tiles hidden behind the zoom. Their most urgent mark badges
   *  the "+N hidden" hint (and names their states in words) so zooming never buries
   *  a notification. */
  hiddenAttentionScope?: DotScope;
  /** SW-EW-46 (D2/D6): the tab ids currently paned as grid cells — marks them
   *  "in view" in the bar so the bar ⇄ grid link is legible. */
  panedTabIds?: readonly string[];
  /** SW-EW-46: whether this workspace is currently showing all its tabs at once
   *  (expanded AND paned) — presses the "Expanded · all tabs" toggle. */
  gridActive?: boolean;
  /** SW-EW-46 (D1): whether this workspace is expanded (zoomed solo or the lone
   *  tile) and may therefore grid its terminals. Gates the dial affordances. */
  canGrid?: boolean;
  /** SW-EW-46 (D2): pane a single tab out as a grid cell / return a cell to the
   *  bar / show all tabs at once / one tab at a time. Absent → no dial (bare
   *  chrome / tests). */
  onPaneOutTab?: (tabId: string) => void;
  onDemotePane?: (tabId: string) => void;
  onShowAllTabs?: () => void;
  onShowOneTab?: () => void;
}

/** Split tabs into a visible window + overflow, ALWAYS keeping the active tab
 *  visible so the focused terminal is never left without a tab pill (the active
 *  tab is pinned into the last visible slot when it would fall into overflow).
 *  Exported for testing. */
export function partitionTabs(
  tabs: TerminalTab[],
  activeTabId: string | undefined,
  max: number,
): { visible: TerminalTab[]; overflow: TerminalTab[] } {
  if (tabs.length <= max) return { visible: tabs, overflow: [] };
  if (max <= 0) return { visible: [], overflow: tabs };
  const head = tabs.slice(0, max);
  if (head.some((t) => t.id === activeTabId)) {
    return { visible: head, overflow: tabs.slice(max) };
  }
  const active = tabs.find((t) => t.id === activeTabId);
  if (!active) return { visible: head, overflow: tabs.slice(max) };
  const visible = [...tabs.slice(0, max - 1), active];
  const visibleIds = new Set(visible.map((t) => t.id));
  return { visible, overflow: tabs.filter((t) => !visibleIds.has(t.id)) };
}

/** A registry shortcut as a menu item's hint: drawn, and announced. */
function keyHint(shortcut: string): KeyHint | undefined {
  return shortcut
    ? { display: formatKeystrokeDisplay(shortcut), aria: ariaKeyshortcuts(shortcut) }
    : undefined;
}

export function TerminalTileChrome({
  workspace: ws,
  teamKey: teamKeyProp,
  isFocused,
  focusIndicator,
  hideIdentity = false,
  glyph,
  glyphColor,
  tabs,
  activeTabId,
  chromeBg = "#161b22",
  terminalBg = "#0d1117",
  onSelectTab,
  onCloseTab,
  onHideTab,
  onEndTab,
  onRestartTab,
  onEndOtherTabs,
  onEndAllTabs,
  onNewShell,
  onNewShellOn,
  onSplitTab,
  onReorderTab,
  onRenameTab,
  onHideTile,
  onCloseTile,
  onFocusTile,
  onOpenStoryDoc,
  onStoryStatusChange,
  isZoomed = false,
  onToggleZoom,
  hiddenTileCount = 0,
  hiddenAttentionScope,
  panedTabIds,
  gridActive = false,
  canGrid = false,
  onPaneOutTab,
  onDemotePane,
  onShowAllTabs,
  onShowOneTab,
}: TerminalTileChromeProps) {
  // SLW-FX-8: surface the REAL, rebindable restore keystroke on the ⛶ tooltip
  // (was a stale "· Esc" — Esc is no longer a restore path). Reuses the same
  // shortcut-display pattern the view-options menu uses for its item hints.
  const zoomKeystroke = formatKeystrokeDisplay(useEffectiveShortcut("viewer.toggle-terminal-zoom"));
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; tabId: string } | null>(
    null,
  );
  const [overflowOpen, setOverflowOpen] = useState(false);
  const rowIdBase = useId();
  // SW-EW-18 AC-6: per-tile story-status dropdown open state (one per tile).
  const [statusOpen, setStatusOpen] = useState(false);
  const { verbFor, probe: probeTabVerb, probeWorkspace: probeTileVerb } = useCloseVerb();
  const handleContextMenu = useCallback(
    (tabId: string, x: number, y: number) => {
      // Learn which verb the tab's key close takes, to mark it in the menu.
      const tab = tabs.find((t) => t.id === tabId);
      if (tab) probeTabVerb(tabId, [tab.sessionId]);
      setContextMenu({ x, y, tabId });
    },
    [tabs, probeTabVerb],
  );
  // A rename (the tab context menu's Rename, or F2) arrives through the shared
  // rename-request store; the tile that shows the tab takes it — once — and hands
  // the tab bar an imperative token, which enters inline-edit mode. The nonce lets
  // repeated requests on the same tab re-arm the editor.
  const [renameRequest, setRenameRequest] = useState<TabRenameRequest | null>(null);
  const tabIdsKey = tabs.map((t) => t.id).join("\n");
  useEffect(() => {
    const ids = new Set(tabIdsKey.split("\n"));
    return useTabRenameRequestStore.subscribe(({ request }) => {
      if (!request || !ids.has(request.tabId)) return;
      useTabRenameRequestStore.setState({ request: null });
      setRenameRequest(request);
    });
  }, [tabIdsKey]);
  const requestRename = useCallback((tabId: string) => requestTabRename(tabId), []);
  const renameShortcut = useEffectiveShortcut("viewer.rename-terminal-tab");
  const newTabShortcut = useEffectiveShortcut("viewer.new-terminal-tab");
  const newOnShortcut = useEffectiveShortcut("viewer.new-terminal-on-machine");
  const splitShortcut = useEffectiveShortcut("viewer.split-terminal");
  // The right-clicked tab's menu offers only what works for it: Restart once
  // its process ended; the tile's doc when it has one; the editor (named after
  // the configured app) when one is set and the workspace has its directory.
  const menuTab = contextMenu ? tabs.find((t) => t.id === contextMenu.tabId) : undefined;
  const menuTabEnded = useSessionEnd(menuTab?.sessionId ?? "", menuTab?.exit) !== null;
  const menuAgentLook = useTabAgentLook(menuTab?.sessionId);
  const menuSessions = useAgentListProjectionStore((s) => s.sessions);
  const menuAgent = menuTab
    ? menuSessions?.find((r) => isFleetAgent(r) && r.sessionId === menuTab.sessionId)
    : undefined;
  const { requestStop, dialog: stopDialog } = useAgentStop();
  // SW-AGR-6: "Remind this agent…" on an interactive agent tab (saved or
  // unnamed, local or remote); its popover hangs from that tab's pill.
  const menuTimerAgent = useTabTimerAgent(menuTab?.sessionId);
  const [remindTabId, setRemindTabId] = useState<string | null>(null);
  const remindTab = remindTabId ? tabs.find((t) => t.id === remindTabId) : undefined;
  const remindAgent = useTabTimerAgent(remindTab?.sessionId);
  const editor = useEditorChoice();
  const editorSet = editor.id !== "" && (editor.id !== "custom" || editor.customCmd.trim() !== "");
  const detectedEditors = useDetectedEditors(!!contextMenu && editorSet && editor.id !== "custom");
  // The toggle says where a press takes the workspace: all its tabs at once, or
  // back to one tab at a time.
  const allTabsLabel = gridActive ? "Back to Expanded" : "Expanded · all tabs";
  const splitKeystroke = formatKeystrokeDisplay(splitShortcut);

  // SW-EW-25 AC-1: an EPIC (integration) workspace has no `storyId` — left to
  // its own devices the chrome would render the raw `integration:/…/epic.md`
  // path id. `useWorkspaceLabel` shows the epic doc's real H1 title (matching
  // the epic HUD heading), falling back to the prettified epic-dir leaf
  // (`epic-workspaces` → `Epic Workspaces`) until the title resolves — through
  // the SAME label span a story tile uses.
  const isEpic = ws.kind === "epic";
  const label = useWorkspaceLabel(ws);
  const activeTeamKey = useActiveTeamKey();
  const teamKey = teamKeyProp ?? activeTeamKey;
  const teamHue = useWorkspaceTeamHue(teamKey, ws.id);
  // The tile `×` names the verb a click takes (Hide / End), probed on hover,
  // focus, or when ⋮ opens (a folded entry has no hover).
  // Fit the bar to the tile's real width: what shows inline, what folds into
  // "+N" or ⋮. Every tab's status stays on screen whichever way it folds.
  const [chromeRef, chromeWidth] = useMeasuredWidth();
  const showGridToggle =
    canGrid && (gridActive || tabs.length >= 2) && Boolean(onShowAllTabs || onShowOneTab);
  const hasHiddenHint = isZoomed && hiddenTileCount > 0;
  const fit = fitTileChrome({
    width: chromeWidth,
    tabCount: tabs.length,
    maxTabs: isZoomed ? MAX_VISIBLE_TABS_ZOOMED : MAX_VISIBLE_TABS,
    hasIdentity: !hideIdentity,
    // Split lives in ⋮ too, so every tile with a Split has the menu.
    hasViewOptions: hasViewOptions(ws.kind) || Boolean(onSplitTab),
    windowControlCount:
      Number(showGridToggle) +
      Number(Boolean(onToggleZoom)) +
      Number(Boolean(onHideTile)) +
      Number(Boolean(onCloseTile)),
    hasHiddenHint,
  });
  const { visible: visibleTabs, overflow: overflowTabs } = useMemo(
    () => partitionTabs(tabs, activeTabId, fit.visibleTabs),
    [tabs, activeTabId, fit.visibleTabs],
  );
  const overflowSessionIds = useMemo(() => overflowTabs.map((t) => t.sessionId), [overflowTabs]);
  const allSessionIds = useMemo(() => tabs.map((t) => t.sessionId), [tabs]);
  const rollup = useScopeMark({ sessionIds: allSessionIds });

  // Window controls folded into ⋮ when the tile is too narrow for them inline.
  const foldedControls: FoldedControl[] = fit.collapseControls
    ? ([
        showGridToggle && {
          key: "grid",
          icon: LayoutGrid,
          label: allTabsLabel,
          onSelect: () => (gridActive ? onShowOneTab?.() : onShowAllTabs?.()),
        },
        onToggleZoom && {
          key: "zoom",
          icon: isZoomed ? Minimize : Maximize,
          label: isZoomed ? "Back to workspaces" : `Expand tile · ${zoomKeystroke}`,
          onSelect: onToggleZoom,
        },
        onHideTile && {
          key: "hide",
          icon: Minus,
          label: "Hide · keeps running",
          onSelect: onHideTile,
        },
        onCloseTile && {
          key: "close",
          icon: X,
          label: closeTooltip(verbFor(ws.id)),
          onSelect: onCloseTile,
        },
      ].filter(Boolean) as FoldedControl[])
    : [];

  // The tab bar renders only `visibleTabs`, but reorder operates on the full
  // per-workspace array — translate the bar's slice-relative indices to
  // full-array indices via tab id so reorder is identity-anchored, not
  // position-anchored (correct even when the active tab is pinned in).
  const handleReorder = (fromIdx: number, toIdx: number) => {
    const fromId = visibleTabs[fromIdx]?.id;
    const toId = visibleTabs[toIdx]?.id;
    if (fromId == null || toId == null) return;
    const from = tabs.findIndex((t) => t.id === fromId);
    const to = tabs.findIndex((t) => t.id === toId);
    if (from === -1 || to === -1) return;
    onReorderTab(from, to);
  };

  return (
    <>
      <div
        ref={chromeRef}
        data-testid={`tile-chrome-${ws.id}`}
        data-focused={isFocused ? "" : undefined}
        data-focus-indicator={focusIndicator}
        onMouseDown={onFocusTile}
        // AC-1 (locked rule 6): double-clicking the chrome bar toggles zoom
        // (desktop muscle memory). Tab rename double-click stops propagation, so
        // it never reaches here; the controls cluster handles its own clicks.
        onDoubleClick={onToggleZoom}
        // SW-TP CAP-12: the chrome row no longer carries the focus-blue bottom
        // border — that signal moved to the per-tab caption so the focused tab's
        // workspace is what reads "focused," not the whole tile chrome. The
        // neutral `border-b` separator stays for row separation.
        //
        // The row is the TAB BAR: `cs-terminal-tabbar` paints it a step darker
        // than the terminal surface (design tokens in index.css), so it reads
        // apart from the app title bar above it in every app theme. The
        // active tab takes the caption's colour below it, joining the two.
        data-surface="terminal"
        className="cs-terminal-tabbar flex items-stretch h-8 border-b flex-shrink-0"
        style={{ ["--cs-tile-terminal-bg" as string]: terminalBg }}
      >
        {/* SW-EW-18 AC-5/6: identity — STORY-STATUS dropdown (story tiles) + STORY-ID.
            The agent roll-up dot is GONE (per-tab dots already report agent status
            on a foreground tile); this slot now SETS the story's lifecycle status
            (the `WorkspacePill` pattern). Non-story tiles render the id alone. */}
        {/* SW-EW-25 AC-7: this identity block MUST be allowed to shrink (no
            `flex-shrink-0`) so a long label truncates instead of expanding the
            tile and shoving the right controls cluster off-screen. The status
            chip + glyph below stay `flex-shrink-0`; only the label yields. */}
        {/* SW-EW-34 v2 (D8): omitted on a lone console tile — the top switcher
            owns identity there. */}
        {!hideIdentity && (
          // overflow-hidden: when the identity yields its width to status and
          // controls, its fixed icons clip here instead of painting over them.
          <div className="flex items-center gap-1.5 px-2 min-w-0 overflow-hidden">
            {showsStoryStatus(ws) && (
              <span
                data-testid={`tile-story-status-${ws.id}`}
                className="flex-shrink-0"
                // Keep the dropdown's clicks from reaching the chrome's focus
                // (onMouseDown) / zoom (onDoubleClick) gestures.
                onMouseDown={(e) => e.stopPropagation()}
                onDoubleClick={(e) => e.stopPropagation()}
              >
                <StatusSelectorPopover
                  currentStatus={ws.storyStatus}
                  onSelect={(newStatus) => {
                    setStatusOpen(false);
                    onStoryStatusChange?.(newStatus);
                  }}
                  open={statusOpen}
                  onOpenChange={setStatusOpen}
                  disabled={!onStoryStatusChange}
                >
                  <StageRail status={ws.storyStatus} />
                </StatusSelectorPopover>
              </span>
            )}
            {/* SW-EW-35: an epic's identity glyph is its DIR icon (`FolderKanban`,
              the same icon the pipeline nav + HUD use for an epic), NOT a
              rolled-up lifecycle-status dot. An epic isn't a single settable
              story, so a status icon here read as a stray blue dot; agent
              activity belongs on the per-tab notification dots (like a story
              tile), not the identity slot. */}
            {isEpic && (
              <span data-testid={`tile-epic-icon-${ws.id}`} className="flex-shrink-0">
                <FolderKanban size={12} className="flex-shrink-0 text-muted-foreground" />
              </span>
            )}
            <RepoGlyph glyph={glyph} glyphColor={glyphColor} slug={ws.sourceRepoSlug} />
            {/* SW-EW-12 AC-10/AC-11: focused → brighter + bolder label; double-click
              opens the workspace doc — a story file or (SW-EW-35) an epic README
              — and recenters the pipeline. A single click leaves the tile-focus
              (onMouseDown) untouched. `stopPropagation` so the dblclick doesn't
              ALSO reach the chrome's `onDoubleClick={onToggleZoom}` (SW-EW-4) and
              zoom the tile. */}
            <MiddleTruncate
              text={label}
              data-testid={`tile-label-${ws.id}`}
              className={`text-xs min-w-0 select-none ${
                isFocused ? "text-foreground font-semibold" : "text-muted-foreground"
              } ${canOpenWorkspaceDoc(ws, onOpenStoryDoc) ? "cursor-pointer" : ""}`}
              title={ws.storyTitle ?? label}
              onDoubleClick={(e) => {
                // Stop the dblclick from also reaching the chrome's
                // onDoubleClick={onToggleZoom} (SW-EW-4) only when we actually open
                // a doc — a doc-less tile's dblclick still falls through to zoom.
                if (openWorkspaceDoc(ws, onOpenStoryDoc)) e.stopPropagation();
              }}
            />
            <WorkspaceMachineMark workspace={ws} testId={`tile-machine-${ws.id}`} />
            {/* SW-EW-35: the `Epic` badge was dropped — the leading dir icon
              already marks an epic tile, so the badge was redundant chrome eating
              horizontal space. The story-HUD toggle moved to the right controls
              cluster (below) to keep the identity block compact. */}
          </div>
        )}

        {/* Tile roll-up: when even the "+N" folds into ⋮, the tile itself carries
            the highest-priority status of all its tabs. */}
        {fit.showRollup && !isQuietMark(rollup.mark) && rollup.mark && (
          <span
            data-testid={`tile-rollup-${ws.id}`}
            className="flex flex-shrink-0 items-center justify-center self-center"
            style={{ width: CHROME_ROLLUP_W }}
          >
            <StatusMark mark={rollup.mark} size="sm" label={rollup.label} />
          </span>
        )}

        {/* AC-6: tabs live IN the chrome. Once measured it isn't clipped here:
            the strip reserves room for exactly the pills the fit hands it, plus
            the "+". (Unmeasured, it clips rather than paint over the controls.) */}
        <div
          className={`flex items-end flex-1 min-w-0 ${chromeWidth === null ? "overflow-hidden" : ""}`}
          style={
            chromeWidth !== null
              ? { minWidth: visibleTabs.length * TAB_PILL_MIN_W + CHROME_NEW_TAB_W }
              : undefined
          }
        >
          <TerminalTabBar
            pillMinWidth={chromeWidth !== null ? TAB_PILL_MIN_W : undefined}
            tabs={visibleTabs}
            activeTabId={activeTabId}
            activeTabBg={chromeBg}
            chromeBg="var(--cs-terminal-tabbar-bg)"
            teamHue={teamHue}
            onSelectTab={onSelectTab}
            onCloseTab={onCloseTab}
            onNewShell={onNewShell}
            onNewShellOn={onNewShellOn}
            newShellOnKeystroke={keyHint(newOnShortcut)?.display}
            onReorderTab={handleReorder}
            onRenameTab={onRenameTab}
            onContextMenu={handleContextMenu}
            menuOpenTabId={contextMenu?.tabId}
            renameRequest={renameRequest}
            onRenameDone={() => focusWorkspaceTerminal(ws.id)}
          />
        </div>

        {/* Right controls — overflow "+N", view options ⋮, grid, zoom, minimize,
            close. Every control is flex-shrink-0 at a declared width, and the fit
            folds grid/zoom/–/× into ⋮ before the tile runs out of room. The
            cluster stops mousedown from bubbling to the chrome's onFocusTile so
            clicking close/promote doesn't transiently focus a tile being torn
            down (relevant to the close-confirm path). */}
        <div
          data-testid={`tile-controls-${ws.id}`}
          className="flex items-center self-center flex-shrink-0 relative"
          style={{ paddingInline: CHROME_CLUSTER_PAD_W / 2 }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          {overflowTabs.length > 0 && !fit.tabsInMenu && (
            <Popover open={overflowOpen} onOpenChange={setOverflowOpen}>
              <PopoverTrigger asChild>
                <TileOverflowButton
                  data-testid={`tile-overflow-${ws.id}`}
                  statusTestId={`tile-overflow-status-${ws.id}`}
                  sessionIds={overflowSessionIds}
                />
              </PopoverTrigger>
              <PopoverContent
                data-testid={`tile-overflow-menu-${ws.id}`}
                align="end"
                side="bottom"
                className="min-w-[140px] gap-0 rounded border border-border bg-popover p-0 py-1 shadow-lg ring-0"
              >
                {overflowTabs.map((t, i) => (
                  <button
                    key={t.id}
                    data-testid={`tile-overflow-tab-${t.id}`}
                    aria-labelledby={`${rowIdBase}-${i}-name ${rowIdBase}-${i}-status`}
                    onClick={() => {
                      onSelectTab(t.id);
                      setOverflowOpen(false);
                    }}
                    className="flex w-full items-center gap-1.5 px-2 py-1 text-left text-xs text-muted-foreground hover:bg-muted hover:text-foreground"
                  >
                    <SessionStatusMark
                      wordsId={`${rowIdBase}-${i}-status`}
                      sessionId={t.sessionId}
                      exit={t.exit}
                      size="sm"
                    />
                    <span id={`${rowIdBase}-${i}-name`} className="truncate">
                      {t.name}
                    </span>
                  </button>
                ))}
              </PopoverContent>
            </Popover>
          )}
          {/* The tile's view toggles (story summary / source diff / hydration)
              collapse into one ⋮ menu, which also takes the window controls and
              (narrowest) the tab list when the tile can't fit them inline. */}
          {fit.showKebab && (
            <TileViewOptionsMenu
              workspace={ws}
              teamKey={teamKey}
              foldedControls={foldedControls}
              onOpen={() => probeTileVerb(ws.id)}
              foldedTabs={fit.tabsInMenu ? tabs : undefined}
              activeTabId={activeTabId}
              onSelectTab={onSelectTab}
              onSplit={onSplitTab && activeTabId ? () => onSplitTab(activeTabId) : undefined}
              splitKeystroke={splitKeystroke}
            />
          )}
          {/* SW-EW-46 (D2): the "Expanded · all tabs" toggle — every tab at once ⇄
              one tab at a time. Showing all tabs needs ≥2 terminals; going back
              stays available whenever they show (even a lone cell, e.g. after
              closing down to one), so the user is never stranded. The same
              PRESET_RING chord (⌘⇧L) then cycles the arrangement. */}
          {showGridToggle && !fit.collapseControls && (
            <InfoTip label={allTabsLabel} side="top">
              <button
                data-testid={`tile-grid-toggle-${ws.id}`}
                aria-label="Expanded · all tabs"
                aria-pressed={gridActive}
                onClick={() => (gridActive ? onShowOneTab?.() : onShowAllTabs?.())}
                className={cn(CHROME_ICON_BUTTON, gridActive && "bg-foreground/10 text-foreground")}
                style={{ width: CHROME_CONTROL_W }}
              >
                <LayoutGrid size={12} aria-hidden="true" />
              </button>
            </InfoTip>
          )}
          {/* AC-1: "+N hidden" hint while zoomed — badged when a hidden tile
              needs you (AC-4 zoom case; reuses the status color vocab). Compact
              ("+N" + dot) when the tile is narrow. */}
          {hasHiddenHint && (
            <span
              data-testid={`tile-hidden-hint-${ws.id}`}
              title={`${hiddenTileCount} tile(s) hidden behind the expanded view`}
              className="relative flex flex-shrink-0 items-center justify-center whitespace-nowrap text-[10px] text-muted-foreground"
              style={{
                width: fit.compactHiddenHint ? CHROME_HIDDEN_HINT_COMPACT_W : CHROME_HIDDEN_HINT_W,
              }}
            >
              {fit.compactHiddenHint ? `+${hiddenTileCount}` : `+${hiddenTileCount} hidden`}
              {hiddenAttentionScope && (
                <ScopedStatusMark
                  scope={hiddenAttentionScope}
                  context="Hidden tiles"
                  quiet="hide"
                  size="sm"
                  className="absolute -top-0.5 -right-0.5"
                  data-testid={`tile-hidden-attention-${ws.id}`}
                />
              )}
            </span>
          )}
          {/* AC-1: maximize/restore zoom toggle (offered at N≥2 — the chrome only
              renders in split). Corner-bracket Maximize/Minimize — visually
              distinct from the Minus minimize-to-card below. */}
          {onToggleZoom && !fit.collapseControls && (
            <InfoTip
              label={isZoomed ? "Back to workspaces" : `Expand tile · ${zoomKeystroke}`}
              side="top"
            >
              <button
                data-testid={`tile-zoom-${ws.id}`}
                onClick={onToggleZoom}
                aria-label={isZoomed ? `Back to workspaces from ${label}` : `Expand ${label}`}
                className={CHROME_ICON_BUTTON}
                style={{ width: CHROME_CONTROL_W }}
              >
                {isZoomed ? <Minimize size={12} /> : <Maximize size={12} />}
              </button>
            </InfoTip>
          )}
          {/* Window convention — [ □ zoom ] [ – Hide ] [ ✕ ], ✕ rightmost. Hide
              (–) moves the tile to the tray, still running. ✕ is the default
              close: Hide when a command runs in the foreground, End otherwise —
              its tooltip names which. Both fold into ⋮ with the same verbs. */}
          {onHideTile && !fit.collapseControls && (
            <InfoTip label="Hide · keeps running" side="top">
              <button
                data-testid={`tile-hide-${ws.id}`}
                onClick={onHideTile}
                aria-label={`Hide ${label}`}
                className={CHROME_ICON_BUTTON}
                style={{ width: CHROME_CONTROL_W }}
              >
                <Minus size={12} />
              </button>
            </InfoTip>
          )}
          {onCloseTile && !fit.collapseControls && (
            <InfoTip label={closeTooltip(verbFor(ws.id))} side="top">
              <button
                data-testid={`tile-close-${ws.id}`}
                onPointerEnter={() => probeTileVerb(ws.id)}
                onFocus={() => probeTileVerb(ws.id)}
                onClick={onCloseTile}
                aria-label={`${verbFor(ws.id)} ${label}`}
                className={CHROME_ICON_BUTTON}
                style={{ width: CHROME_CONTROL_W }}
              >
                <X size={12} />
              </button>
            </InfoTip>
          )}
        </div>
      </div>

      <TabContextMenu
        contextMenu={contextMenu}
        onClose={() => setContextMenu(null)}
        onHideTab={onHideTab}
        onEndTab={onEndTab}
        onStopAgent={menuAgent ? () => requestStop(menuAgent) : undefined}
        stopAgentDisabledReason={
          menuTab?.origin?.offlineSince !== undefined
            ? "This machine is offline: the agent can be stopped once it reconnects."
            : menuTabEnded || (menuAgent && !menuAgent.alive)
              ? "This session has ended."
              : undefined
        }
        onRestartTab={menuTabEnded && !menuTab?.origin ? onRestartTab : undefined}
        doc={
          canOpenWorkspaceDoc(ws, onOpenStoryDoc)
            ? {
                label:
                  getKindCapabilities(ws.kind).docPathSource === "epicDocPath"
                    ? "Open Epic Doc"
                    : "Open Story Doc",
                run: () => openWorkspaceDoc(ws, onOpenStoryDoc),
              }
            : undefined
        }
        editor={
          editorSet && ws.cwd && ws.status === "ready"
            ? {
                label: editorLaunchLabel(editor.id, editor.customCmd, detectedEditors),
                run: () => openInEditorFromCwd(ws.cwd),
              }
            : undefined
        }
        keyCloseVerb={contextMenu ? verbFor(contextMenu.tabId) : undefined}
        onEndOtherTabs={onEndOtherTabs}
        onEndAllTabs={onEndAllTabs}
        canGrid={canGrid}
        tabCount={tabs.length}
        panedTabIds={panedTabIds}
        onPaneOut={onPaneOutTab}
        onDemotePane={onDemotePane}
        onRenameTab={requestRename}
        renameKeystroke={keyHint(renameShortcut)}
        onNewTab={onNewShell}
        newTabKeystroke={keyHint(newTabShortcut)}
        onNewTabOn={onNewShellOn}
        newTabOnKeystroke={keyHint(newOnShortcut)}
        onSplit={onSplitTab}
        splitKeystroke={keyHint(splitShortcut)}
        agentLook={menuAgentLook}
        onRemind={menuTimerAgent && !menuTabEnded ? setRemindTabId : undefined}
      />
      {stopDialog}
      {remindTab && (
        <RemindPopover
          open
          onOpenChange={(open) => {
            if (!open) setRemindTabId(null);
          }}
          target={
            remindTab.origin
              ? { sessionId: remindTab.sessionId, machineId: remindTab.origin.machineId }
              : { sessionId: remindTab.sessionId }
          }
          agentName={remindAgent?.saved ? remindAgent.name : null}
          offline={remindTab.origin?.offlineSince !== undefined}
          workspaceCwd={ws.cwd}
          anchor={document.querySelector<HTMLElement>(
            `[data-testid="terminal-tab-${CSS.escape(remindTab.id)}"]`,
          )}
        />
      )}
    </>
  );
}

// ---------------------------------------------------------------------------
// TileViewOptionsMenu — the per-tile "view options" overflow (⋮). Collects the
// tile's view toggles (story summary / source diff / hydration) into one kebab
// so the chrome stays legible as more toggles are added. Each entry is a
// descriptor gated by the workspace kind's capability flag; the kebab hides
// entirely when the kind qualifies for none. Adding a future toggle is one more
// descriptor — no chrome-geometry change. Radix DropdownMenu supplies the
// menuitemcheckbox role, roving focus, type-ahead, and Escape-to-close.
// ---------------------------------------------------------------------------

interface TileViewOption {
  key: string;
  icon: LucideIcon;
  label: string;
  shown: boolean;
  /** Rebindable keystroke display, or a falsy value (null for hydration, or the
   *  empty string `useEffectiveShortcut` returns when an action is unbound) when
   *  there is no hint to show. */
  shortcut: string | null;
  onToggle: () => void;
  testId: string;
}

function TileViewOptionsMenu({
  workspace: ws,
  teamKey,
  foldedControls = [],
  foldedTabs,
  activeTabId,
  onSelectTab,
  onOpen,
  onSplit,
  splitKeystroke,
}: {
  workspace: Workspace;
  /** The team the workspace belongs to. */
  teamKey: string;
  /** Split the active tab: a new terminal beside it, side by side. */
  onSplit?: () => void;
  splitKeystroke?: string;
  /** The menu opened — lets the tile probe its folded close verb. */
  onOpen?: () => void;
  activeTabId?: string;
  /** Window controls the tile is too narrow to show inline. */
  foldedControls?: FoldedControl[];
  /** The tile's tabs, listed here when even the "+N" trigger doesn't fit. */
  foldedTabs?: TerminalTab[];
  onSelectTab?: (tabId: string) => void;
}) {
  const rowIdBase = useId();
  // What had focus before the menu opened: the source diff hands focus back to
  // it on hide (the menu item itself unmounts with the menu).
  const [preMenuFocus, setPreMenuFocus] = useState<Element | null>(null);
  const caps = getKindCapabilities(ws.kind);
  // Hooks run unconditionally over the fixed toggle set; the capability flags
  // below decide which descriptors actually render. Actions are read via
  // getState() (stable refs — no per-tile subscription needed).
  const storyHudShown = useStoryHudShown(ws.id);
  // Per-tab visibility keys on sessionId — fall back to a benign empty string
  // on a tile with no active tab yet (toggle actions below bail on falsy).
  const sourceDiffShown = useSourceDiffShown(activeTabId);
  const toggleSourceDiff = useCallback(
    (sessionId: string) => {
      if (!sessionId) return;
      // A real prior target only (not body).
      if (
        !sourceDiffShown &&
        preMenuFocus instanceof HTMLElement &&
        preMenuFocus !== document.body
      ) {
        setSourceDiffReturnFocus(sessionId, preMenuFocus);
      }
      getViewerState().toggleSourceDiffShown({ sessionId, workspaceId: ws.id, teamKey });
    },
    [sourceDiffShown, preMenuFocus, ws.id, teamKey],
  );
  const codeEditorOpen = useWorktreeEditorOpen(activeTabId);
  const hudShortcut = useEffectiveShortcut("viewer.toggle-story-hud");
  const sourceDiffShortcut = useEffectiveShortcut("viewer.toggle-source-diff");
  // SLW-DV-3: the code-editor view toggle's rebindable hint.
  const codeEditorShortcut = useEffectiveShortcut("viewer.toggle-code-editor");

  const options: TileViewOption[] = [
    caps.hudToggle && {
      key: "story-hud",
      icon: PanelTopOpen,
      // The HUD renders the epic README on an epic tile, the story doc otherwise.
      label: ws.kind === "epic" ? "Epic summary" : "Story summary",
      shown: storyHudShown,
      shortcut: hudShortcut,
      onToggle: () => getViewerState().toggleStoryHudShown(ws.id),
      testId: `story-hud-toggle-${ws.id}`,
    },
    // SW-VE-12: source-diff tracks `diffCapable` (root/story/generic);
    // code-editor tracks `sourceCapable` (every live kind). The overlay
    // surfaces its own empty-state matrix downstream (no git repo /
    // no changes / loading / failed).
    caps.diffCapable &&
      activeTabId && {
        key: "source-diff",
        icon: FileDiff,
        label: "Source diff",
        shown: sourceDiffShown,
        shortcut: sourceDiffShortcut,
        onToggle: () => toggleSourceDiff(activeTabId),
        testId: `source-diff-toggle-${ws.id}`,
      },
    caps.sourceCapable &&
      activeTabId && {
        key: "code-editor",
        icon: Code,
        label: "Code editor",
        shown: codeEditorOpen,
        shortcut: codeEditorShortcut,
        onToggle: () =>
          getViewerState().toggleWorktreeEditorOpen({
            sessionId: activeTabId,
            workspaceId: ws.id,
            teamKey,
          }),
        testId: `code-editor-toggle-${ws.id}`,
      },
  ].filter(Boolean) as TileViewOption[];

  const hasFolded = foldedControls.length > 0 || (foldedTabs?.length ?? 0) > 0;
  // Never render an empty kebab.
  if (options.length === 0 && !hasFolded && !onSplit) return null;
  const onlyViewOptions = !hasFolded && !onSplit;

  return (
    <DropdownMenu
      onOpenChange={(open) => {
        if (!open) return;
        // Opened from the keyboard, this is the menu's own trigger: it stays
        // mounted, so it is a better return target than none (body).
        setPreMenuFocus(document.activeElement);
        onOpen?.();
      }}
    >
      <DropdownMenuTrigger
        data-testid={`tile-view-options-${ws.id}`}
        aria-label={onlyViewOptions ? "View options" : "Tile options"}
        // A native title matches the sibling `⌄ +N` overflow trigger and avoids
        // nesting a tooltip Slot inside the menu trigger.
        title={onlyViewOptions ? "View options" : "Tile options"}
        // A double-click on the kebab must NOT bubble to the chrome's
        // onDoubleClick={onToggleZoom} (it would zoom the tile); the cluster
        // already stops mousedown from reaching onFocusTile.
        onDoubleClick={(e) => e.stopPropagation()}
        style={{ width: CHROME_CONTROL_W }}
        className={cn(
          CHROME_ICON_BUTTON,
          "data-[state=open]:bg-foreground/10 data-[state=open]:text-foreground",
        )}
      >
        <MoreVertical size={12} />
      </DropdownMenuTrigger>
      <DropdownMenuContent
        data-testid={`tile-view-options-menu-${ws.id}`}
        align="end"
        side="bottom"
      >
        {foldedTabs?.map((t, i) => (
          <DropdownMenuItem
            key={t.id}
            aria-labelledby={`${rowIdBase}-${i}-name ${rowIdBase}-${i}-status`}
            data-testid={`tile-menu-tab-${t.id}`}
            aria-current={t.id === activeTabId ? "true" : undefined}
            onSelect={() => onSelectTab?.(t.id)}
            className={cn(t.id === activeTabId && "font-medium")}
          >
            <span className="flex w-4 justify-center">
              <SessionStatusMark
                wordsId={`${rowIdBase}-${i}-status`}
                sessionId={t.sessionId}
                exit={t.exit}
                size="sm"
              />
            </span>
            <span id={`${rowIdBase}-${i}-name`} className="truncate">
              {t.name}
            </span>
            {t.id === activeTabId && <Check className="ml-auto" aria-hidden />}
          </DropdownMenuItem>
        ))}
        {foldedTabs &&
          foldedTabs.length > 0 &&
          (onSplit || foldedControls.length > 0 || options.length > 0) && <DropdownMenuSeparator />}
        {onSplit && (
          <DropdownMenuItem data-testid={`tile-menu-split-${ws.id}`} onSelect={onSplit}>
            <Columns2 />
            <span>Split</span>
            {splitKeystroke ? <DropdownMenuShortcut>{splitKeystroke}</DropdownMenuShortcut> : null}
          </DropdownMenuItem>
        )}
        {onSplit && (foldedControls.length > 0 || options.length > 0) && <DropdownMenuSeparator />}
        {foldedControls.map((c) => {
          const Icon = c.icon;
          return (
            <DropdownMenuItem
              key={c.key}
              data-testid={`tile-menu-${c.key}-${ws.id}`}
              onSelect={c.onSelect}
            >
              <Icon />
              <span>{c.label}</span>
            </DropdownMenuItem>
          );
        })}
        {foldedControls.length > 0 && options.length > 0 && <DropdownMenuSeparator />}
        {options.map((o) => {
          const Icon = o.icon;
          return (
            <DropdownMenuCheckboxItem
              key={o.key}
              data-testid={o.testId}
              checked={o.shown}
              // Selecting closes the menu via Radix's default (no onSelect
              // preventDefault); onCheckedChange still fires the toggle — so
              // clicking the currently-open overlay's row closes it. A leading
              // check (ItemIndicator) marks the open overlay; only one is ever
              // checked at a time (the overlays are mutually exclusive).
              onCheckedChange={o.onToggle}
              className={cn(o.shown && "font-medium")}
            >
              <Icon />
              <span>{o.label}</span>
              {o.shortcut ? (
                <DropdownMenuShortcut>{formatKeystrokeDisplay(o.shortcut)}</DropdownMenuShortcut>
              ) : null}
            </DropdownMenuCheckboxItem>
          );
        })}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/**
 * The tile's overflow "+N": the hidden tabs' most urgent status as a dot, and
 * in words in its name and tooltip. Its own component, so the label's age
 * ticks re-render only this button.
 */
function TileOverflowButton({
  statusTestId,
  sessionIds,
  ...rest
}: {
  statusTestId: string;
  sessionIds: readonly string[];
} & ComponentProps<"button">) {
  // The name holds the words; the tooltip adds the state's age.
  const { mark, label, words } = useScopeMark({ sessionIds });
  const loud = !isQuietMark(mark);
  const count = sessionIds.length;
  return (
    <button
      {...rest}
      title={overflowLabel(count, ["tab", "tabs"], loud ? label : undefined)}
      aria-label={overflowLabel(count, ["tab", "tabs"], loud ? words : undefined)}
      className={cn(
        CHROME_ICON_BUTTON,
        "relative text-xs data-[state=open]:bg-foreground/10 data-[state=open]:text-foreground",
      )}
      style={{ width: CHROME_OVERFLOW_W }}
    >
      <span className="relative flex-shrink-0">
        <ChevronDown size={12} />
        {/* The hidden tabs' most urgent mark, in a tab pill's mark's
            place and size. */}
        {loud && mark && (
          <StatusMark
            mark={mark}
            size="sm"
            className="absolute -top-0.5 -right-0.5"
            data-testid={statusTestId}
          />
        )}
      </span>
      +{sessionIds.length}
    </button>
  );
}
