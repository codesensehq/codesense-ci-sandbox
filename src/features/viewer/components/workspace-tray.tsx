// SPDX-License-Identifier: MIT
import { registerNativeMenu } from "@/features/shell/native-menu-openers";
import { WorkspaceGroupMenu } from "./workspace-group-menu";
import { customGroupId } from "../lib/workspace-groups";
import { openLaunchSheet } from "@/features/agents/lib/launch-sheet";
import { Fragment, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ChevronDown, EyeOff, Plus, X, Rows3, Layers } from "lucide-react";
import { useDraggable } from "@dnd-kit/core";
import type { PipelineStatus, DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { selectHiddenTerminalTabs, useViewerStore } from "@/shared/stores/viewer-store";
import { applyPillSelection } from "@/features/viewer/lib/agents-pane-pill";
import {
  useMinimizedTerminalTabIndex,
  useTeamTerminalTabs,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import { useTeamWorkspaces } from "@/shared/hooks/use-team-workspaces";
import { useHiddenTerminalTabs, useScopeMark } from "../hooks/use-active-workspace-terminal-tabs";
import {
  trayWorkspaceOrder,
  epicRailEntries,
  epicChildEntries,
  railKeyOf,
  railInfo,
  type TrayWorkspaceEntry,
  type EpicRailEntry,
} from "../lib/group-workspaces-by-epic";
import { trayChipDragId } from "../lib/terminal-drag";
import { buildTeamOrder, stepSlot, type SlotCard } from "../lib/workspace-order";
import {
  clearTrayMoveRefused,
  moveWorkspaceFromTray,
  useTrayMoveRefused,
} from "../lib/tray-reorder-drag";
import { clearTrayMove, useLastTrayMove } from "@/shared/lib/tray-move-announcement";
import { useTrayReorder } from "../hooks/use-tray-reorder";
import { closeWorkspace, endWorkspace, hideWorkspace, useCloseVerb } from "../lib/terminal-close";
import {
  activateAndFocusTrayWorkspace,
  revealCollapsedTerminal,
  selectAndResumeEpicRail,
  subscribeForegroundWorkspaceActivation,
} from "../lib/focus-cascade";
import { isTerminalColumnShown } from "../lib/terminal-column";
import {
  measuredZoom,
  resolveWorkspaceBarSize,
  workspaceBarZoom,
} from "@/shared/lib/workspace-bar-size";
import { getEpicSpineColor } from "@/shared/lib/pipeline-graph/epic-colors";
import {
  PositionedMenuItem,
  type ContextMenuPosition,
} from "@/shared/components/positioned-context-menu";
import { useWorkspaceCrews, type PillCrew } from "../hooks/use-workspace-crews";
import { usePillLabel, WorkspacePill } from "./workspace-pill";
import { formatKeystrokeDisplay, isMac } from "@/shared/lib/shortcuts/keystroke";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { ScopedStatusMark } from "./session-status-mark";
import { HiddenTabMenuItems } from "./hidden-tab-menu-items";
import {
  RemoveWorktreeDialog,
  removeWorktreeTargetFor,
  type RemoveWorktreeTarget,
} from "./remove-worktree-dialog";
import { useWorktreeRosterStore } from "../stores/worktree-roster-store";
import { StatusMark } from "@/shared/components/status-mark";
import { isQuietMark } from "@/shared/lib/status-mark";
import { startDragging } from "@/shared/lib/titlebar";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { CHROME_ICON, CHROME_ICON_BUTTON } from "@/shared/components/chrome-control";
import { overflowLabel } from "../lib/tile-chrome-fit";
import type { Workspace } from "@/shared/stores/viewer-store";
import { useToastBottomClearance } from "@/shared/hooks/use-toast-bottom-clearance";

/** SW-CW-2: structural selection — the selected chip/pill is the one filled,
 *  elevated, bolder item (see `.cs-workspace-card-selected` in viewer-themes.css).
 *  Shared by the tray chip and the epic-rail pill so the "this is selected"
 *  affordance can't drift, and legible with color fully removed (replaces the
 *  old `#388bfd` accent ring). */
const SELECTED_CARD_CLASS = "cs-workspace-card-selected";

/** SW-CW-2: the 2px left bar carries exactly one meaning at a time — a NEUTRAL
 *  full-contrast bar when selected (the sole selection signal, wins over the
 *  epic hue), else the desaturated epic spine (a faint grouping hint), else none
 *  (roots have no epic). One helper so the tray chip and epic-rail pill can't
 *  diverge. The neutral color is the theme's own sidebar text (the same token
 *  the selected surface mixes toward) so the bar stays theme-consistent, with
 *  `--foreground` as the cross-theme fallback. Exported for unit coverage of the
 *  precedence contract. */
export function selectionBarStyle(
  active: boolean,
  spineColor?: string,
): React.CSSProperties | undefined {
  if (active) return { borderLeft: "2px solid var(--cs-sidebar-text, var(--foreground))" };
  return spineColor ? { borderLeft: `2px solid ${spineColor}` } : undefined;
}
/** Shared styling for the tray's plain controls, at the strip's 32px control height. */
const TRAY_CONTROL_BTN_CLASS =
  "flex h-8 flex-shrink-0 items-center justify-center rounded px-2 text-muted-foreground hover:bg-accent/40 hover:text-foreground focus-visible:outline focus-visible:outline-1 focus-visible:outline-accent";
/** Stable empty-rail reference so the flat-mode `rail` memo keeps a constant
 *  identity (no churn in downstream memos/effects). */
const EMPTY_RAIL: EpicRailEntry[] = [];

export interface WorkspaceTrayProps {
  teamKey: string;
  /** Full pipeline tree — feeds epic grouping + the spine identity color. */
  tree: DerivedPipelineTree | null;
  /** sourceRepoSlug → 2-char glyph (only when the team has 2+ source repos). */
  glyphMap?: Map<string, string>;
  isLightTheme: boolean;
  /** Open the new-workspace quick-switch picker (the trailing "+" launcher). */
  onNewWorkspace: () => void;
  onContextMenu?: (workspaceId: string, minimized: boolean, x: number, y: number) => void;
  onStoryStatusChange?: (workspaceId: string, newStatus: PipelineStatus) => void;
  onOpenStoryDoc?: (storyFilePath: string) => void;
  /** SW-EW-35: right-pinned CLI controls (layout / full-screen / epic-repos)
   *  hosted on the tray's strip so the CLI has ONE bottom chrome, not a tray
   *  strip plus a separate controls strip. Stays out of the scrolling chip
   *  area. */
  trailing?: React.ReactNode;
  /** A control pinned to the strip's left end, before the pills (the Agents control). It
   *  rides the story strip — the flat strip, or the grouped story row — never the epic rail. */
  leading?: React.ReactNode;
  /** SW-EW-34 v2: when the tray is portaled into the title bar (console mode's
   *  workspace switcher) it must blend into the top chrome — drop the
   *  bottom-strip top-border + muted background. Default (bottom placement)
   *  keeps the SW-EW-33 strip chrome. */
  topChrome?: boolean;
  /** SW-EW-38: in console (`topChrome`) mode the tray's top part (the flat
   *  switcher, or the epic rail when grouped) is portaled into this title-bar
   *  slot, while the grouped story row renders inline at the bottom. Null until
   *  the title bar registers the slot → the tray renders nothing. */
  consoleRailSlot?: HTMLElement | null;
}

interface TrayChipProps {
  entry: TrayWorkspaceEntry;
  index: number;
  isActive: boolean;
  /** SW-FX-52: the chip self-subscribes to its own workspace's mark via
   *  `useScopeMark`, so a status tick re-renders only the affected chip — not the
   *  whole tray (AC #2). */
  teamKey: string;
  glyph?: string;
  liveTerminalCount: number;
  statusPopoverOpen: boolean;
  onStatusPopoverOpenChange: (open: boolean) => void;
  onSelect: (workspaceId: string, via?: { keyboard: boolean }) => void;
  onClose: (workspaceId: string) => void;
  onContextMenu?: (workspaceId: string, minimized: boolean, x: number, y: number) => void;
  onStoryStatusChange?: (workspaceId: string, newStatus: PipelineStatus) => void;
  onOpenStoryDoc?: (storyFilePath: string) => void;
  /** The agents working in this workspace, shown on the chip. */
  crew?: PillCrew;
  /** Crowded strip: render the compact pill (never for the focused chip). */
  compact?: boolean;
  /** In the one-line strip the chip keeps its width (the row overflows
   *  into compact pills and the "+N" instead of squeezing labels). */
  oneLine?: boolean;
}

/**
 * One tray chip — a drag source (namespaced `tray:{id}` so it can't collide with
 * the in-mosaic tile draggable for the same on-screen workspace, Seam D) wrapping
 * the shared `WorkspacePill`. The selected pill's neutral left bar and the faint
 * position numeral (first 9, the `Cmd+1..9` legend) are tray-only chrome; a
 * workspace pill wears no epic hue.
 */
function TrayChip({
  entry,
  index,
  isActive,
  teamKey,
  glyph,
  liveTerminalCount,
  statusPopoverOpen,
  onStatusPopoverOpenChange,
  onSelect,
  onClose,
  onContextMenu,
  onStoryStatusChange,
  onOpenStoryDoc,
  crew,
  compact = false,
  oneLine = false,
}: TrayChipProps) {
  const ws = entry.workspace;
  // SW-FX-52: scalar per-workspace subscription — this chip re-renders only when
  // ITS status changes, not on every other session's tick.
  const markScope = useMemo(() => ({ teamKey, wsIds: [ws.id] }), [teamKey, ws.id]);
  // The `×` names the verb a click takes (Hide / End), probed on hover.
  const { verbFor, probeWorkspace } = useCloseVerb();
  // Tabs Hidden from this tile: a count badge (aggregate status) that pulses as
  // one arrives (keyed by the count, so each change replays the short pulse), and
  // opens the chip menu listing them for restore.
  const hiddenTabs = useHiddenTerminalTabs(teamKey, ws.id);
  // Both on-screen (rearrange) and minimized (restore-into-split) chips drag.
  const { setNodeRef, listeners, isDragging } = useDraggable({ id: trayChipDragId(ws.id) });
  // SW-EW-33 AC-2 / SW-CW-2 — three instantly-distinguishable states: focused =
  // structural filled/elevated/bolder card (exactly one, legible without color);
  // on-screen = flat filled; minimized = subdued decorative card chrome.
  // Status marks, text and focus controls stay at full opacity.
  const stateClass = isActive
    ? SELECTED_CARD_CLASS
    : entry.minimized
      ? "cs-workspace-card-minimized"
      : "";
  return (
    <div
      ref={setNodeRef}
      {...(listeners ?? {})}
      data-tray-chip={ws.id}
      data-minimized={entry.minimized ? "true" : "false"}
      data-active={isActive ? "true" : "false"}
      data-compact={compact && !isActive ? "true" : undefined}
      // Keep the pill's minimum height and intrinsic width; crowding uses compact
      // pills and overflow rather than shrinking every workspace label.
      // One-line: `cs-tray-chip-one-line` caps the pill to the strip (the query container), less
      // room for its edge fades, so even a full active pill can be scrolled into its lane.
      className={`relative flex min-h-7 ${oneLine ? "cs-tray-chip-one-line flex-shrink-0" : ""} items-stretch self-center rounded cs-workspace-card ${stateClass} ${
        isDragging ? "opacity-40" : ""
      }`}
      style={{
        // No epic hue on a workspace pill (the strip's rules and the rail carry the grouping); the
        // selected pill keeps the neutral bar the selected epic pill wears.
        ...selectionBarStyle(isActive),
      }}
    >
      <WorkspacePill
        workspace={ws}
        isActive={isActive}
        markScope={markScope}
        liveTerminalCount={liveTerminalCount}
        closeVerb={verbFor(ws.id)}
        onCloseHover={() => probeWorkspace(ws.id)}
        dimmed={entry.minimized}
        glyph={glyph}
        isStatusPopoverOpen={statusPopoverOpen}
        onStatusPopoverOpenChange={onStatusPopoverOpenChange}
        onSelect={onSelect}
        onClose={onClose}
        onContextMenu={(id, x, y) => onContextMenu?.(id, entry.minimized, x, y)}
        onStoryStatusChange={onStoryStatusChange}
        onOpenStoryDoc={onOpenStoryDoc}
        navRow
        crew={crew}
        compact={compact && !isActive}
        oneLine={oneLine}
        description={oneLine ? pillMoveKeys() : undefined}
        shortcutHint={index < 9 && <TrayShortcutHint n={index + 1} />}
        hiddenTabsControl={
          hiddenTabs.length > 0 && (
            <HiddenTabsBadge
              teamKey={teamKey}
              workspaceId={ws.id}
              count={hiddenTabs.length}
              onOpen={(x, y) => onContextMenu?.(ws.id, entry.minimized, x, y)}
            />
          )
        }
      />
    </div>
  );
}

/** Ids of the items in `container` (elements carrying the `data-${attr}`
 *  attribute) that are not fully inside its visible box — scrolled or wrapped
 *  out of view. Re-measured on scroll, when the container OR any item resizes
 *  (a chip going bold, a late-arriving label), and whenever the item list
 *  changes (`listKey`). Exported for unit coverage. */
export function useClippedTrayItems(
  container: HTMLElement | null,
  attr: "tray-chip" | "epic-rail-pill",
  listKey: string,
): string[] {
  const [clipped, setClipped] = useState<string[]>([]);
  useLayoutEffect(() => {
    // No strip mounted → nothing renders the "+N", so stale ids are harmless.
    if (!container) return;
    const selector = `[data-${attr}]`;
    const measure = () => {
      const box = container.getBoundingClientRect();
      const ids: string[] = [];
      container.querySelectorAll<HTMLElement>(selector).forEach((item) => {
        const r = item.getBoundingClientRect();
        const outside =
          r.left < box.left - 0.5 ||
          r.right > box.right + 0.5 ||
          r.top < box.top - 0.5 ||
          r.bottom > box.bottom + 0.5;
        const id = item.getAttribute(`data-${attr}`);
        if (outside && id) ids.push(id);
      });
      setClipped((prev) => (prev.join("\n") === ids.join("\n") ? prev : ids));
    };
    measure();
    container.addEventListener("scroll", measure, { passive: true });
    const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    ro?.observe(container);
    container.querySelectorAll(selector).forEach((item) => ro?.observe(item));
    return () => {
      container.removeEventListener("scroll", measure);
      ro?.disconnect();
    };
  }, [container, attr, listKey]);
  return clipped;
}

/** Which ends of a one-row strip have chips scrolled past them. Re-read on
 *  scroll, when the strip or a chip resizes, and when `listKey` changes (the
 *  caller folds the compact state into it). Exported for unit coverage. */
export function useScrollEdges(
  container: HTMLElement | null,
  listKey: string,
  enabled = true,
): "none" | "start" | "end" | "both" {
  const [measured, setEdges] = useState<"none" | "start" | "end" | "both">("none");
  useLayoutEffect(() => {
    if (!container || !enabled) return;
    const read = () => {
      const start = container.scrollLeft > 1;
      const end = container.scrollLeft + container.clientWidth < container.scrollWidth - 1;
      setEdges(start && end ? "both" : start ? "start" : end ? "end" : "none");
    };
    read();
    container.addEventListener("scroll", read, { passive: true });
    // The container's box rarely changes when chips do (it flexes): watch the
    // chips too, so a chip growing or going compact re-reads the ends.
    const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(read);
    ro?.observe(container);
    container.querySelectorAll("[data-tray-chip]").forEach((c) => ro?.observe(c));
    return () => {
      container.removeEventListener("scroll", read);
      ro?.disconnect();
    };
  }, [container, listKey, enabled]);
  return container && enabled ? measured : "none";
}

/** A soft fade over the strip's scrolled-off ends (a mask, so it works on any
 *  surface and theme without a color). */
const FADE_PX = 16;
const FADE = `${FADE_PX}px`;
const EDGE_MASK: Record<"start" | "end" | "both", string> = {
  start: `linear-gradient(to right, transparent, #000 ${FADE})`,
  end: `linear-gradient(to left, transparent, #000 ${FADE})`,
  both: `linear-gradient(to right, transparent, #000 ${FADE}, #000 calc(100% - ${FADE}), transparent)`,
};

/** Scroll a one-row strip — only the strip, never the page or viewer around
 *  it — so `chip` sits fully inside it and clear of the edge fade. The rects are
 *  drawn (viewport) px and the fade and `scrollBy` are the strip's own px: at a
 *  scaled bar size they differ by the strip's measured zoom. */
function scrollChipClear(area: HTMLElement, chip: HTMLElement): void {
  const zoom = measuredZoom(area);
  const box = area.getBoundingClientRect();
  const r = chip.getBoundingClientRect();
  // A pill nearly as wide as the strip (its tightest step) keeps an even margin instead, so a
  // focus inside it can't keep shifting it under the pointer.
  const m = Math.min(FADE_PX * zoom, Math.max(0, (box.width - r.width) / 2));
  const delta =
    r.left < box.left + m
      ? r.left - box.left - m
      : r.right > box.right - m
        ? r.right - box.right + m
        : 0;
  if (delta !== 0) area.scrollBy({ left: delta / zoom });
}

/** How long a chip click waits, with the CLI closed, for a second click that
 *  makes it a double-click (open the doc) instead of a reopen. */
const DOUBLE_CLICK_MS = 250;

/** Extra width (the strip's own px) it must gain over what the full pills
 *  needed before compact pills go back to full: about one "+N" button. Scaled
 *  by the strip's measured zoom, since the widths it is added to are drawn px. */
const COMPACT_RELEASE_MARGIN = 56;

/** Whether the one-line strip is crowded: true once its chips overflow at
 *  full size, so every chip but the focused one renders compact. It remembers
 *  how wide the full chips needed to be and only goes back to full pills once
 *  the bar is at least that wide again — compacting makes the row fit, so
 *  "does it fit now?" alone would flip back and forth. Exported for unit
 *  coverage. */
export function useCompactWhenCrowded(
  container: HTMLElement | null,
  listKey: string,
  enabled = true,
): boolean {
  const [compact, setCompact] = useState(false);
  const needed = useRef(0);
  // A changed chip list starts over at full size and re-measures (still before
  // paint): the remembered width belonged to the old list.
  const [prevListKey, setPrevListKey] = useState(listKey);
  if (prevListKey !== listKey) {
    setPrevListKey(listKey);
    setCompact(false);
  }
  // Disabling measurement forgets the compact state.
  if (!enabled && compact) setCompact(false);
  const active = compact && enabled && container !== null;
  useLayoutEffect(() => {
    if (!container || !enabled) return;
    const measure = () => {
      const box = container.getBoundingClientRect();
      if (compact) {
        // The margin covers the "+N" that shows beside an overflowing full row
        // and vanishes once compact pills fit: without it, losing the "+N" could
        // widen the strip just enough to flip back, overflow, and flip again.
        const margin = COMPACT_RELEASE_MARGIN * measuredZoom(container);
        if (box.width >= needed.current + margin) setCompact(false);
        return;
      }
      let left = box.left;
      let right = box.left;
      let overflows = false;
      container.querySelectorAll<HTMLElement>("[data-tray-chip]").forEach((chip) => {
        const r = chip.getBoundingClientRect();
        left = Math.min(left, r.left);
        right = Math.max(right, r.right);
        if (r.right > box.right + 0.5 || r.left < box.left - 0.5) overflows = true;
      });
      if (overflows) {
        needed.current = right - left;
        setCompact(true);
      }
    };
    measure();
    const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    ro?.observe(container);
    // At full size a chip can grow in place (a late label, a crew joining).
    if (!compact) container.querySelectorAll("[data-tray-chip]").forEach((c) => ro?.observe(c));
    return () => ro?.disconnect();
  }, [container, listKey, enabled, compact]);
  return active;
}

/**
 * A chip's Hidden-tabs badge: the count, and their most urgent mark (a crash or
 * a clean exit included: a hidden tab whose process ended stays listed) with the
 * tabs named by state in its name and tooltip; opens the chip menu listing them.
 */
function HiddenTabsBadge({
  teamKey,
  workspaceId,
  count,
  onOpen,
}: {
  teamKey: string;
  workspaceId: string;
  count: number;
  onOpen: (x: number, y: number) => void;
}) {
  const { mark, label: tip, words } = useScopeMark({ teamKey, hiddenOf: workspaceId });
  const ended = mark === "crashed" || mark === "ended";
  const shows = mark !== undefined && (ended || !isQuietMark(mark));
  const label = `${count} hidden tab${count === 1 ? "" : "s"} — restore${shows ? `, ${words}` : ""}`;
  const lead = ended
    ? "Hidden tabs — one has ended. Click to see."
    : "Hidden tabs — still running. Click to restore.";
  return (
    <button
      type="button"
      data-testid={`tray-chip-hidden-${workspaceId}`}
      aria-label={label}
      title={`${lead}${shows ? `\n${tip}` : ""}`}
      onPointerDown={(e) => e.stopPropagation()}
      onClick={(e) => {
        const r = e.currentTarget.getBoundingClientRect();
        e.stopPropagation();
        onOpen(r.left, r.bottom);
      }}
      className="cs-workspace-hidden-control relative grid items-center justify-center gap-x-0.5 self-center rounded px-1 text-[length:calc(10px*var(--bar-floor,1))] tabular-nums text-muted-foreground hover:bg-accent focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring"
    >
      <EyeOff size={10} aria-hidden className="cs-workspace-hidden-eye shrink-0" />
      <span
        className="cs-workspace-hidden-count"
        data-testid={`tray-chip-hidden-count-${workspaceId}`}
      >
        {count}
      </span>
      {shows && (
        <span className="cs-workspace-hidden-mark-envelope">
          <StatusMark
            mark={mark}
            size="sm"
            data-testid={
              ended
                ? `tray-chip-hidden-ended-${workspaceId}`
                : `tray-chip-hidden-status-${workspaceId}`
            }
          />
        </span>
      )}
    </button>
  );
}

/** A workspace's name as its pill shows it. */
function WorkspaceName({ workspace }: { workspace: Workspace }) {
  return <>{usePillLabel(workspace)}</>;
}

/** The pill's jump key (`Cmd+N` on a Mac, `Ctrl+N` elsewhere, unless rebound), as Settings →
 *  Shortcuts binds it: the hint follows a rebind. A long binding truncates inside the end slot. */
function TrayShortcutHint({ n }: { n: number }) {
  const binding = useEffectiveShortcut(`viewer.jump-workspace-${n}`);
  let shown = "";
  try {
    shown = formatKeystrokeDisplay(binding);
  } catch {
    // A stale saved binding that no longer parses shows no hint.
  }
  if (!shown) return null;
  return (
    <span
      aria-hidden
      data-tray-shortcut=""
      title={shown}
      // `--bar-floor`: never smaller than at Normal on a Small bar (agents.css).
      className="cs-tray-shortcut pointer-events-none text-[length:calc(10px*var(--bar-floor,1))]"
    >
      {shown}
    </span>
  );
}
/** The key that moves a focused pill, as this platform names it: Option (⌥) on a Mac. */
const moveModifierName = () => (isMac() ? "Option" : "Alt");
/** The keys that move a focused pill, in its accessible description. */
const pillMoveKeys = () => {
  const mod = moveModifierName();
  return `${mod}+Left or ${mod}+Right moves it along the bar.`;
};

/** How long a move's words stay in the live region before they are cleared. */
const MOVE_ANNOUNCED_MS = 3000;

/** A polite live region that says where the last tray move (a drop or Alt+Left/Right) put the
 *  pill: "Moved X after Y". Keyed by the move, so the same words twice are announced twice. Once
 *  announced the move is cleared, and a region that mounts drops any move made before it, so a
 *  tray that mounts again never repeats an old move. */
function TrayMoveAnnouncer({
  teamKey,
  entries,
}: {
  teamKey: string;
  entries: readonly TrayWorkspaceEntry[];
}) {
  const latest = useLastTrayMove(teamKey);
  // The move standing when this region mounted is not this region's to announce (the region is
  // keyed by its team, so a team switch mounts a new one).
  const [stale] = useState(() => latest?.seq ?? null);
  const move = latest && latest.seq !== stale ? latest : null;
  useEffect(() => {
    if (stale !== null) clearTrayMove(teamKey, stale);
  }, [stale, teamKey]);
  useEffect(() => {
    if (!move) return;
    const timer = window.setTimeout(() => clearTrayMove(teamKey, move.seq), MOVE_ANNOUNCED_MS);
    return () => window.clearTimeout(timer);
  }, [move, teamKey]);
  // A drop in the band whose move could not be made (the bar changed under the drag) is said too.
  const refusedNow = useTrayMoveRefused(teamKey);
  const [staleRefusal] = useState(() => refusedNow?.seq ?? null);
  const refused = refusedNow && refusedNow.seq !== staleRefusal ? refusedNow : null;
  useEffect(() => {
    if (staleRefusal !== null) clearTrayMoveRefused(teamKey, staleRefusal);
  }, [staleRefusal, teamKey]);
  useEffect(() => {
    if (!refused) return;
    const timer = window.setTimeout(
      () => clearTrayMoveRefused(teamKey, refused.seq),
      MOVE_ANNOUNCED_MS,
    );
    return () => window.clearTimeout(timer);
  }, [refused, teamKey]);
  const live = useViewerStore((s) => s.workspaces[teamKey]);
  const minimized = useViewerStore((s) => s.softClosedWorkspaces[teamKey]);
  // A workspace the strip no longer draws (another epic's, in grouped mode, or one closed since)
  // is still named by its own record; one with none left by its id.
  const byId = (id: string): Workspace | string =>
    entries.find((e) => e.workspace.id === id)?.workspace ??
    live?.find((w) => w.id === id) ??
    minimized?.[id]?.workspace ??
    id;
  const name = (w: Workspace | string) =>
    typeof w === "string" ? w : <WorkspaceName workspace={w} />;
  const moved = move ? byId(move.workspaceId) : undefined;
  const before = move ? "before" in move.place : false;
  const anchor = move
    ? byId("before" in move.place ? move.place.before : move.place.after)
    : undefined;
  return (
    <div
      role="status"
      aria-live="polite"
      className="sr-only"
      data-testid="workspace-tray-move-status"
    >
      {move && moved && anchor && (
        <span key={move.seq}>
          Moved {name(moved)} {before ? "before" : "after"} {name(anchor)}
        </span>
      )}
      {refused && (
        <span key={`refused-${refused.seq}`}>
          Couldn't move {name(byId(refused.workspaceId))}: the bar changed during the drag
        </span>
      )}
    </div>
  );
}

/** One row of the tray's "+N" list: status dot + label, click to activate. */
function HiddenChipRow({
  teamKey,
  workspace,
  onSelect,
}: {
  teamKey: string;
  workspace: Workspace;
  onSelect: (workspaceId: string) => void;
}) {
  const label = usePillLabel(workspace);
  const id = useId();
  return (
    <button
      type="button"
      data-testid={`workspace-tray-hidden-row-${workspace.id}`}
      aria-labelledby={`${id}-name ${id}-status`}
      onClick={() => onSelect(workspace.id)}
      className="flex w-full items-center gap-1.5 rounded px-2 py-1 text-left text-xs hover:bg-accent/40"
    >
      <ScopedStatusMark wordsId={`${id}-status`} scope={{ teamKey, wsIds: [workspace.id] }} />
      <span id={`${id}-name`} className="truncate">
        {label}
      </span>
    </button>
  );
}

/** One row of the epic rail's "+N" list: the epic's roll-up dot + title. */
function HiddenRailRow({
  teamKey,
  rail,
  onSelect,
}: {
  teamKey: string;
  rail: EpicRailEntry;
  onSelect: (rail: EpicRailEntry) => void;
}) {
  const memberIds = useMemo(() => rail.members.map((m) => m.workspace.id), [rail.members]);
  const id = useId();
  return (
    <button
      type="button"
      data-testid={`workspace-epic-rail-hidden-row-${rail.railKey}`}
      aria-labelledby={`${id}-name ${id}-status`}
      onClick={() => onSelect(rail)}
      className="flex w-full items-center gap-1.5 rounded px-2 py-1 text-left text-xs hover:bg-accent/40"
    >
      <ScopedStatusMark wordsId={`${id}-status`} scope={{ teamKey, wsIds: memberIds }} />
      <span id={`${id}-name`} className="truncate">
        {rail.title}
      </span>
      <span className="ml-auto text-[11px] tabular-nums opacity-60">{rail.members.length}</span>
    </button>
  );
}

/** A tray "+N" for items scrolled or wrapped out of view: carries the hidden
 *  workspaces' highest-priority status and lists the hidden items. */
function TrayOverflowButton({
  teamKey,
  count,
  noun,
  statusWorkspaceIds,
  testId,
  menuZoom = 1,
  children,
}: {
  teamKey: string;
  count: number;
  noun: [singular: string, plural: string];
  /** The workspaces whose aggregate status badges the "+N". */
  statusWorkspaceIds: readonly string[];
  testId: string;
  /** The bar's zoom when the "+N" sits in the scaled bottom bar: its menu (portaled
   *  out of the bar) is drawn at the same size. */
  menuZoom?: number;
  children: (close: () => void) => React.ReactNode;
}) {
  const [open, setOpen] = useState(false);
  // The name holds the words; the tooltip adds the state's age.
  const { mark, label, words } = useScopeMark({ teamKey, wsIds: statusWorkspaceIds });
  const loud = !isQuietMark(mark);
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          data-testid={testId}
          aria-label={overflowLabel(count, noun, loud ? words : undefined)}
          title={overflowLabel(count, noun, loud ? label : undefined)}
          // Equal-width digits keep count changes from resizing the lane and
          // flipping a fractionally clipped chip between visible and hidden.
          className={`${TRAY_CONTROL_BTN_CLASS} self-start gap-0.5 text-xs tabular-nums`}
        >
          <span className="relative flex-shrink-0">
            <ChevronDown className="h-3.5 w-3.5" />
            {loud && mark && (
              <StatusMark
                mark={mark}
                size="sm"
                className="absolute -top-0.5 -right-0.5"
                data-testid={`${testId}-status`}
              />
            )}
          </span>
          +{count}
        </button>
      </PopoverTrigger>
      <PopoverContent
        onCloseAutoFocus={(event) => {
          const focused = document.activeElement;
          if (focused && focused !== document.body && focused.isConnected) {
            event.preventDefault();
          }
        }}
        data-testid={`${testId}-menu`}
        align="end"
        className="w-56 gap-0 overflow-y-auto p-1"
        // A long list scrolls inside the window instead of running off it. The
        // available height is viewport px; inside the zoomed menu, its own px.
        style={{
          zoom: menuZoom === 1 ? undefined : menuZoom,
          maxHeight: `calc(var(--radix-popover-content-available-height, 100vh) / ${menuZoom})`,
        }}
      >
        {children(() => setOpen(false))}
      </PopoverContent>
    </Popover>
  );
}

/**
 * SW-EW-38 — one epic-rail pill (grouped mode, row 1): a group SELECTOR derived
 * from a rail key, NOT a `Workspace` (not draggable). Renders the epic title, a
 * roll-up status mark (the most urgent mark across the group's members, by the
 * shared `topMark`, its members named by state in words), and a child count.
 * Exactly one pill is selected (the structural `SELECTED_CARD_CLASS` treatment);
 * a group with no on-screen member is dimmed. Click → select the epic +
 * MRU-resume (handled by the caller).
 */
function EpicRailPill({
  rail,
  selected,
  teamKey,
  spineColor,
  tabIndex,
  buttonRef,
  onSelect,
  onContextMenu,
}: {
  rail: EpicRailEntry;
  selected: boolean;
  teamKey: string;
  spineColor?: string;
  /** OC-6: roving tab stop — `0` for the active pill, `-1` for the rest, so the
   *  rail is a single Tab stop with ArrowLeft/Right moving focus within it. */
  tabIndex: number;
  /** OC-6: register the pill's button so the roving handler can move DOM focus +
   *  scroll it into view without a state-mirroring effect. */
  buttonRef: (el: HTMLButtonElement | null) => void;
  onSelect: (rail: EpicRailEntry) => void;
  onContextMenu?: (x: number, y: number) => void;
}) {
  // SW-FX-52: the rail roll-up self-subscribes to just ITS members' sessions via
  // a string-keyed selector, so a member's status tick re-renders only this
  // pill — not the tray.
  const memberWsIds = useMemo(() => rail.members.map((m) => m.workspace.id), [rail.members]);
  const stateClass = selected
    ? SELECTED_CARD_CLASS
    : rail.hasOpen
      ? ""
      : "cs-workspace-card-minimized";
  const id = useId();
  return (
    <button
      type="button"
      id={id}
      ref={buttonRef}
      data-epic-rail-pill={rail.railKey}
      data-selected={selected ? "true" : "false"}
      aria-label={`${railInfo(teamKey, rail.railKey).kind === "epic" ? "Epic" : "Group"} ${rail.title}, ${rail.members.length} workspace${
        rail.members.length === 1 ? "" : "s"
      },`}
      // Named by its own label, then the roll-up dot's words without their
      // ticking age ("Epic X, 2 workspaces, Idle").
      aria-labelledby={`${id} ${id}-status`}
      // OC-6: single-select collection semantics — `aria-current` is the "this is
      // the selected epic" signal (replaces the wrong toggle-button `aria-pressed`).
      aria-current={selected ? "true" : undefined}
      tabIndex={tabIndex}
      onClick={() => onSelect(rail)}
      onContextMenu={
        onContextMenu
          ? (e) => {
              e.preventDefault();
              e.stopPropagation();
              const r = e.currentTarget.getBoundingClientRect();
              onContextMenu(e.clientX || r.left, e.clientY || r.bottom);
            }
          : undefined
      }
      onKeyDown={
        onContextMenu
          ? (e) => {
              if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
                e.preventDefault();
                e.stopPropagation();
                const r = e.currentTarget.getBoundingClientRect();
                onContextMenu(r.left, r.bottom);
              }
            }
          : undefined
      }
      title={rail.title}
      className={`relative flex flex-shrink-0 items-center gap-1.5 rounded px-2 cs-workspace-card whitespace-nowrap focus-visible:outline focus-visible:outline-1 focus-visible:outline-accent ${stateClass}`}
      style={selectionBarStyle(selected, spineColor)}
    >
      <ScopedStatusMark
        wordsId={`${id}-status`}
        scope={{ teamKey, wsIds: memberWsIds }}
        size="sm"
        data-testid={`epic-rail-status-${rail.railKey}`}
      />
      <span className="max-w-[12rem] truncate text-xs">{rail.title}</span>
      <span className="text-[11px] tabular-nums opacity-60">{rail.members.length}</span>
    </button>
  );
}

/**
 * SW-EW-33 — the always-visible workspace tray pinned to the bottom of the
 * CLI/mosaic container. Lists EVERY workspace (open ∪ minimized) as chips in the
 * single-source `trayWorkspaceOrder`, with a trailing "+" launcher. Click routes
 * through the shared `activateTrayWorkspace` state-rule (on-screen → focus,
 * minimized → replace the focused leaf); drag onto a tile edge splits (Seam D).
 * No collapse, no toggle — it never vanishes (zero workspaces → just the "+").
 */
export function WorkspaceTray({
  teamKey,
  tree,
  glyphMap,
  isLightTheme,
  onNewWorkspace,
  onContextMenu,
  onStoryStatusChange,
  onOpenStoryDoc,
  trailing,
  leading,
  topChrome = false,
  consoleRailSlot = null,
}: WorkspaceTrayProps) {
  // Toasts sit above the tray's bottom strips, including the grouped rail.
  const toastClearanceRef = useToastBottomClearance();
  const workspaces = useTeamWorkspaces(teamKey);
  const softClosedForTeam = useViewerStore((s) => s.softClosedWorkspaces[teamKey]);
  const terminalTabsForTeam = useTeamTerminalTabs(teamKey);
  const minimizedTabsForTeam = useMinimizedTerminalTabIndex()[teamKey];
  const crews = useWorkspaceCrews(teamKey);
  const activeWorkspaceId = useViewerStore((s) => s.activeWorkspace[teamKey]);
  const setTrayOrder = useViewerStore((s) => s.setTrayOrder);
  const setTrayFullOrder = useViewerStore((s) => s.setTrayFullOrder);
  const seenCoachmark = useViewerStore((s) => s.workspaceTraySeenCoachmark);
  const markCoachmarkSeen = useViewerStore((s) => s.markWorkspaceTrayCoachmarkSeen);
  // SW-EW-38: tray lens + selected epic-rail key (default flat / "").
  const mode = useViewerStore((s) => s.trayMode[teamKey]) ?? "flat";
  const selectedRaw = useViewerStore((s) => s.selectedEpicDirName[teamKey]) ?? "";
  const setTrayMode = useViewerStore((s) => s.setTrayMode);
  const setSelectedEpicGroup = useViewerStore((s) => s.setSelectedEpicGroup);
  const [statusPopover, setStatusPopover] = useState<string | null>(null);
  // SW-EW-33 (AC-6): right-click chip menu — Hide / Restore + End.
  const [chipMenu, setChipMenu] = useState<{
    id: string;
    minimized: boolean;
    pos: ContextMenuPosition;
    owner?: object;
  } | null>(null);
  const [railMenu, setRailMenu] = useState<{
    id: string;
    pos: ContextMenuPosition;
    owner?: object;
  } | null>(null);
  const externalMenu = useRef<{ owner: object; close: () => void } | null>(null);
  const closeMenu = () => {
    const external = externalMenu.current;
    externalMenu.current = null;
    external?.close();
    setChipMenu(null);
    setRailMenu(null);
  };
  const customGroups = useViewerStore((s) => s.customGroupsByTeam[teamKey]);
  const worktreeRows = useWorktreeRosterStore((s) => s.rows);
  const [removeTarget, setRemoveTarget] = useState<RemoveWorktreeTarget | null>(null);
  // The shared tray survives team switches; transient actions belong only to the team that
  // opened them, even when the destination has workspaces with the same IDs.
  const [transientTeam, setTransientTeam] = useState(teamKey);
  if (transientTeam !== teamKey) {
    setTransientTeam(teamKey);
    setStatusPopover(null);
    setChipMenu(null);
    setRailMenu(null);
    setRemoveTarget(null);
  }

  // The team's one shared order (pills, the Agents pane's cards and the tiles): the computed tray
  // order, with the person's moves applied (a workspace can be moved anywhere, across epics too).
  // Until the tree loads, the last computed order saved for the team stands in for it, as it does
  // for the pane and the tiles (`teamWorkspaceOrder`).
  const storedOrder = useViewerStore((s) => s.workspaceOrderByTeam[teamKey]);
  const savedBase = useViewerStore((s) => (tree ? undefined : s.trayBaseOrderByTeam[teamKey]));
  // A move is applied over the same computed order the pills are drawn from (`drawnBase`, the
  // tree's when the tray has it), so a drop lands where the line showed even before the store's
  // saved base catches up with the tree.
  const { entries, drawnBase } = useMemo(() => {
    const byId = (e: TrayWorkspaceEntry) => e.workspace.id;
    const computed = trayWorkspaceOrder(workspaces, softClosedForTeam ?? {}, tree, customGroups);
    return {
      // A newly opened workspace takes its place beside its own epic's pills.
      entries: buildTeamOrder(computed, byId, savedBase, storedOrder, railKeyOf),
      drawnBase: tree ? computed.map(byId) : undefined,
    };
  }, [workspaces, softClosedForTeam, tree, savedBase, storedOrder, customGroups]);

  // SW-EW-38 (R1): the epic rail and the per-epic slice are pure derivations of
  // the ONE ordered `entries` array — no second grouping/order pass, so rail
  // order, row-2 order, and the published Cmd order can never diverge.
  // SW-EW-38: grouped engages in BOTH placements. In shared mode it's two
  // stacked rows at the bottom; in console mode (`topChrome`) it SPLITS — the
  // epic rail portals into the title-bar slot (top) and the selected epic's
  // story row renders inline at the bottom (see the return below).
  const grouped = mode === "grouped";
  // Gated on `grouped` (R4): flat mode reads neither, so it must pay no
  // per-render rail reduction — important because the status marks tick on every
  // agent status change.
  const rail = useMemo(
    () => (grouped ? epicRailEntries(entries, teamKey, customGroups) : EMPTY_RAIL),
    [grouped, entries, teamKey, customGroups],
  );
  // Resolve the selected rail key with the AC-8 fallback: if the stored key has
  // no live members (epic hard-closed, or stale), fall back to the first pill.
  const selectedRailKey = useMemo(() => {
    if (!grouped) return selectedRaw;
    if (rail.some((r) => r.railKey === selectedRaw)) return selectedRaw;
    return rail[0]?.railKey ?? "";
  }, [grouped, selectedRaw, rail]);
  const children = useMemo(
    () => epicChildEntries(entries, selectedRailKey),
    [entries, selectedRailKey],
  );

  // The array the user actually sees: flat = the full order; grouped = the
  // selected epic's row-2 slice. One source for both the render and the Cmd
  // publish below.
  const visibleEntries = grouped ? children : entries;

  // Chips scrolled or wrapped out of the strip's view surface as a "+N" that
  // carries their aggregate status, so no workspace is ever silently off-screen.
  const [chipArea, setChipArea] = useState<HTMLDivElement | null>(null);
  const chipListKey = useMemo(
    () => visibleEntries.map((e) => e.workspace.id).join("\n"),
    [visibleEntries],
  );
  // Settings → "Workspace bar size": one zoom for the whole bottom bar. A zoom
  // change leaves every chip's layout box (and so its ResizeObserver) unchanged;
  // only the strip's layout width may change, and not in every placement. The
  // size is folded into every measuring hook's key so each re-measures at once.
  const barSize = useViewerStore((s) => resolveWorkspaceBarSize(s.settings.workspaceBarSize));
  const barZoom = workspaceBarZoom(barSize);
  const measureKey = `${chipListKey}|${barSize}`;
  const clippedIds = useClippedTrayItems(chipArea, "tray-chip", measureKey);
  // A pill dragged along the strip reorders: one insertion line between pills marks where it lands.
  const insertion = useTrayReorder(teamKey, chipArea, visibleEntries, drawnBase);
  // At the pill's own place the line stays, dimmed: the drag is live, but a release moves nothing.
  const insertionLine = insertion ? (
    <span
      aria-hidden
      data-testid="workspace-tray-insertion"
      data-at-home={insertion.atHome ? "" : undefined}
      className="cs-tray-insertion"
      style={{ transform: `translateX(${insertion.x}px)` }}
    />
  ) : null;
  // Both placements share the same single-row strip: crowded, unfocused chips
  // go compact, while the focused workspace stays full and in view.
  const compactChips = useCompactWhenCrowded(chipArea, measureKey);
  const chipEdges = useScrollEdges(chipArea, `${measureKey}|${compactChips}`);
  // Tabbing onto a chip brings it clear of the edge fade, as focus-follow does.
  const onChipAreaFocus = (e: React.FocusEvent<HTMLDivElement>) => {
    const chip = (e.target as HTMLElement).closest<HTMLElement>("[data-tray-chip]");
    if (chip && e.currentTarget.contains(chip)) scrollChipClear(e.currentTarget, chip);
  };
  // Alt+Left/Right on a focused pill moves it one place along the strip, as Alt+Up/Down moves a
  // card in the Agents pane. The pill keeps focus and stays in view.
  const onChipAreaKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (!e.altKey || e.metaKey || e.ctrlKey || e.shiftKey) return;
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    const area = e.currentTarget;
    // Only from the pill's own selection button: its other controls (the stage, the Hidden-tabs
    // badge, the close ×) keep Alt+arrows to themselves.
    if (!(e.target as HTMLElement).matches("[data-pill-select]")) return;
    const chip = (e.target as HTMLElement).closest<HTMLElement>("[data-tray-chip]");
    const id = chip?.dataset.trayChip;
    if (!chip || !id || !area.contains(chip)) return;
    e.preventDefault();
    const chips = Array.from(area.querySelectorAll<HTMLElement>("[data-tray-chip]"));
    const shown = new Set(visibleEntries.map((v) => v.workspace.id));
    const cards: SlotCard[] = chips.map((c) => {
      const chipId = c.dataset.trayChip ?? "";
      return { id: chipId, pill: shown.has(chipId) };
    });
    const step = stepSlot(cards, id, e.key === "ArrowLeft" ? "up" : "down");
    if (!step || !moveWorkspaceFromTray(teamKey, id, step.place, drawnBase)) return;
    const focused = e.target as HTMLElement;
    requestAnimationFrame(() => {
      if (!focused.isConnected) return;
      if (document.activeElement !== focused) focused.focus({ preventScroll: true });
      const moved = area.querySelector<HTMLElement>(`[data-tray-chip="${CSS.escape(id)}"]`);
      if (moved) scrollChipClear(area, moved);
    });
  };
  // `--cs-tray-fade`: the edge fade's width, which a one-line pill leaves clear (viewer-themes.css).
  const chipAreaStyle = {
    "--cs-tray-fade": FADE,
    ...(chipEdges === "none"
      ? {}
      : { maskImage: EDGE_MASK[chipEdges], WebkitMaskImage: EDGE_MASK[chipEdges] }),
  } as React.CSSProperties;
  const manualChipScroll = useRef(false);
  const followedWorkspaceId = useRef(`${teamKey}\u0000${activeWorkspaceId}`);
  const [explicitActivation, setExplicitActivation] = useState(0);
  useLayoutEffect(
    () =>
      subscribeForegroundWorkspaceActivation((activatedTeam, id) => {
        if (activatedTeam !== teamKey || id !== useViewerStore.getState().activeWorkspace[teamKey])
          return;
        manualChipScroll.current = false;
        setExplicitActivation((revision) => revision + 1);
      }),
    [teamKey],
  );
  // Keep the focused chip on screen in the one-line bar, wherever focus came from
  // (a click, Cmd+1..9, an agent row). Overflowed chips are counted by the "+N".
  // A layout effect, so the strip is scrolled before the frame paints.
  useLayoutEffect(() => {
    if (followedWorkspaceId.current !== `${teamKey}\u0000${activeWorkspaceId}`) {
      followedWorkspaceId.current = `${teamKey}\u0000${activeWorkspaceId}`;
      manualChipScroll.current = false;
    }
    if (!chipArea || !activeWorkspaceId) return;
    const chips = Array.from(chipArea.querySelectorAll<HTMLElement>("[data-tray-chip]"));
    const chip = chips.find((el) => el.dataset.trayChip === activeWorkspaceId);
    if (!chip) return;
    const reveal = () => {
      if (!manualChipScroll.current) scrollChipClear(chipArea, chip);
    };
    reveal();
    // Chips settle after mount (labels resolve, crews arrive, compact kicks in)
    // and can push the focused chip back out: follow those size changes — until
    // the user scrolls the strip themselves, which is never undone.
    const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(reveal);
    ro?.observe(chipArea);
    chips.forEach((el) => ro?.observe(el));
    const stopFollowing = () => {
      manualChipScroll.current = true;
    };
    chipArea.addEventListener("wheel", stopFollowing, { passive: true });
    chipArea.addEventListener("touchstart", stopFollowing, { passive: true });
    return () => {
      ro?.disconnect();
      chipArea.removeEventListener("wheel", stopFollowing);
      chipArea.removeEventListener("touchstart", stopFollowing);
    };
  }, [chipArea, teamKey, activeWorkspaceId, measureKey, explicitActivation]);
  // Picking a workspace on the bar while the CLI is closed brings the panel back.
  const selectWorkspace = (id: string) => {
    revealCollapsedTerminal();
    // One call activates AND focuses the workspace's terminal.
    activateAndFocusTrayWorkspace(teamKey, id);
    if (!seenCoachmark) markCoachmarkSeen(); // the gesture's been used
  };
  // A chip click with the CLI closed waits out the double-click window first: a
  // double-click opens the workspace's doc in the viewer, and reopening the CLI
  // on its first click would shrink the very viewer the doc opens in. With the
  // CLI open there is nothing to protect, so the click acts at once (OC-5).
  const pendingSelect = useRef<ReturnType<typeof setTimeout> | null>(null);
  const cancelPendingSelect = () => {
    if (pendingSelect.current !== null) clearTimeout(pendingSelect.current);
    pendingSelect.current = null;
  };
  useLayoutEffect(() => cancelPendingSelect, [teamKey]);
  // A pill click also reopens the agents pane's group and shows the workspace's lead tab (see
  // applyPillSelection). The pane effects run with the select, so a double-click that opens the
  // doc instead leaves the pane alone. Other selection paths do not touch the pane.
  const pillSelect = (id: string, via?: { keyboard: boolean }) =>
    selectChip(id, via, () => applyPillSelection(useViewerStore.getState(), teamKey, id));
  const selectChip = (id: string, via?: { keyboard: boolean }, beforeSelect?: () => void) => {
    cancelPendingSelect();
    const select = () => {
      beforeSelect?.();
      selectWorkspace(id);
    };
    // Enter/Space can't become a double-click: act at once, CLI open or closed.
    if (via?.keyboard || isTerminalColumnShown(useViewerStore.getState())) {
      select();
      return;
    }
    pendingSelect.current = setTimeout(() => {
      pendingSelect.current = null;
      select();
    }, DOUBLE_CLICK_MS);
  };
  const openChipDoc = (storyFilePath: string) => {
    cancelPendingSelect(); // the click pair was a double-click: open the doc only
    onOpenStoryDoc?.(storyFilePath);
  };
  const hiddenWorkspaces = useMemo(() => {
    if (clippedIds.length === 0) return [];
    const set = new Set(clippedIds);
    return visibleEntries.filter((e) => set.has(e.workspace.id)).map((e) => e.workspace);
  }, [clippedIds, visibleEntries]);
  const hiddenWorkspaceIds = useMemo(() => hiddenWorkspaces.map((w) => w.id), [hiddenWorkspaces]);

  const [railArea, setRailArea] = useState<HTMLDivElement | null>(null);
  const railListKey = useMemo(() => rail.map((r) => r.railKey).join("\n"), [rail]);
  const clippedRailKeys = useClippedTrayItems(
    railArea,
    "epic-rail-pill",
    `${railListKey}|${barSize}`,
  );
  const hiddenRails = useMemo(() => {
    if (clippedRailKeys.length === 0) return EMPTY_RAIL;
    const set = new Set(clippedRailKeys);
    return rail.filter((r) => set.has(r.railKey));
  }, [clippedRailKeys, rail]);
  const hiddenRailMemberIds = useMemo(
    () => hiddenRails.flatMap((r) => r.members.map((m) => m.workspace.id)),
    [hiddenRails],
  );

  // Publish the rendered chip order so the Cmd+1..9 positional jump indexes the
  // exact array on screen (setTrayOrder no-ops when unchanged → no render loop).
  useEffect(() => {
    setTrayOrder(
      teamKey,
      visibleEntries.map((e) => e.workspace.id),
    );
  }, [teamKey, visibleEntries, setTrayOrder]);

  // SW-CW-4: also publish the FULL strip order (all rails) so `closeWorkspace`
  // can pick the adjacent tab across epic boundaries — in grouped mode
  // `visibleEntries` is only the selected rail's row-2 slice, so it can't.
  // Separate effect keyed on `entries` alone: a rail switch changes only
  // `visibleEntries`, so this doesn't need to re-run when the full order is
  // unchanged.
  useEffect(() => {
    setTrayFullOrder(
      teamKey,
      entries.map((e) => e.workspace.id),
    );
  }, [teamKey, entries, setTrayFullOrder]);

  // SW-EW-38 (AC-5/R5): follow-focus — keep the stored selection in sync with
  // what's displayed. Keyed on BOTH the active workspace AND the mode, so
  // *entering* grouped mode (a trayMode flip, not a focus change) immediately
  // snaps the selection to the focused ws's epic instead of a stale/first pill.
  // The desired key is the focused ws's rail key when it resolves, else the
  // AC-8-resolved `selectedRailKey` (the fallback that the render already shows)
  // — writing that back keeps the store from lingering on a stale/empty epic
  // while the UI displays a different pill. Guarded no-op-when-unchanged → the
  // epic-click → activate → this effect loop settles in one pass.
  useEffect(() => {
    if (!grouped) return;
    const focused = entries.find((e) => e.workspace.id === activeWorkspaceId);
    const key = focused ? railKeyOf(focused) : selectedRailKey;
    if (key !== selectedRaw) setSelectedEpicGroup(teamKey, key);
  }, [
    grouped,
    activeWorkspaceId,
    entries,
    selectedRailKey,
    selectedRaw,
    teamKey,
    setSelectedEpicGroup,
  ]);

  // SW-EW-38 (AC-6) / OC-6: epic-pill click = select + MRU-resume. Routes through
  // the shared `selectAndResumeEpicRail` tail so the pointer path and the keyboard
  // prev/next-epic command can never drift (OC-6 AC5). Selection always moves;
  // it activates (and focuses, via the OC-5 coordinator) only when the rail has an
  // eligible target — so selecting an epic accepts typing without a second click.
  const onSelectEpic = (railEntry: EpicRailEntry) => {
    closeMenu(); // switching epics swaps the story row → close any open chip menu
    const mru = useViewerStore.getState().mruWorkspaceStack[teamKey] ?? [];
    const activated = selectAndResumeEpicRail(
      teamKey,
      railEntry.railKey,
      railEntry.members.map((m) => m.workspace),
      mru,
    );
    // With the CLI closed, an epic that resumed a workspace brings the panel back
    // (its focus lands a frame later, once the terminal is visible). One with
    // nothing to resume only moves the selection.
    if (activated) revealCollapsedTerminal();
    if (activated && !seenCoachmark) markCoachmarkSeen();
  };

  // OC-6 (AC3): rail-local roving focus. The tab stop follows the SELECTED epic by
  // default (realigned in render on any selection change — the React "adjust state
  // during render" pattern, no effect); ArrowLeft/Right/Home/End move it among
  // pills WITHOUT activating (Enter/Space on the focused pill's native button
  // activates). Manual activation so rapid arrow-repeat never thrashes terminal
  // focus (AC7). Focus + keep-visible run in the event handler (a user-caused side
  // effect), not a state-mirroring effect.
  const selectedRailIndex = Math.max(
    0,
    rail.findIndex((r) => r.railKey === selectedRailKey),
  );
  const [railFocusIndex, setRailFocusIndex] = useState(selectedRailIndex);
  const selectedRailIdentity = `${teamKey}\u0000${selectedRailKey}`;
  const [prevSelectedRailKey, setPrevSelectedRailKey] = useState(selectedRailIdentity);
  if (prevSelectedRailKey !== selectedRailIdentity) {
    setPrevSelectedRailKey(selectedRailIdentity);
    setRailFocusIndex(selectedRailIndex);
  }
  const activeRailIndex = rail.length === 0 ? -1 : Math.min(railFocusIndex, rail.length - 1);
  const railButtonRefs = useRef<Map<string, HTMLButtonElement | null>>(new Map());
  const focusRailPill = (index: number) => {
    const clamped = Math.max(0, Math.min(rail.length - 1, index));
    setRailFocusIndex(clamped);
    const el = railButtonRefs.current.get(rail[clamped]?.railKey ?? "");
    el?.focus();
    // Style-only keep-visible (AC7): default (non-smooth) so rapid arrow-repeat
    // can't queue an animation backlog; `nearest` avoids a needless scroll jump.
    el?.scrollIntoView({ block: "nearest", inline: "nearest" });
  };
  const onRailKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    // Only rove when focus is on a pill — arrows on the mode toggle / trailing
    // controls keep their native behavior. Rail arrows CLAMP at the ends
    // (`focusRailPill` clamps; Home/End reach the extremes) — deliberately NOT
    // wrapping, unlike the prev/next-epic COMMANDS which wrap (AC1). The rail is a
    // roving widget (move focus), the command is a jump (cycle); the APG toolbar
    // convention is clamp, so the two edge behaviors differ by design.
    if (!(e.target as HTMLElement).closest("[data-epic-rail-pill]")) return;
    switch (e.key) {
      case "ArrowLeft":
        e.preventDefault();
        focusRailPill(activeRailIndex - 1);
        break;
      case "ArrowRight":
        e.preventDefault();
        focusRailPill(activeRailIndex + 1);
        break;
      case "Home":
        e.preventDefault();
        focusRailPill(0);
        break;
      case "End":
        e.preventDefault();
        focusRailPill(rail.length - 1);
        break;
    }
  };
  // OC-6 (AC4): a polite live region announces the selected epic. A keyboard
  // command moves focus to the terminal (not the pill), so without this an
  // assistive-tech user wouldn't hear the epic change; reflecting the selected
  // title declaratively announces it on every selection change (click or command).
  const selectedRailTitle = rail.find((r) => r.railKey === selectedRailKey)?.title ?? "";

  // DRY (R1): one chip-strip renderer for BOTH the flat row and the grouped
  // row 2. `list` drives the index (the Cmd+1..9 legend) AND the divider lookup,
  // so a grouped row 2 (all one epic) shows no inner dividers, and flat stays
  // byte-identical to today.
  const renderChips = (list: TrayWorkspaceEntry[]) =>
    list.map((entry, i) => {
      const ws = entry.workspace;
      const slug = ws.sourceRepoSlug;
      const group = railInfo(teamKey, railKeyOf(entry));
      // Hairline divider wherever the epic changes (and at a root↔epic boundary). The order is the
      // person's, so the same epic can come up more than once, each run set off by its own rule.
      const prev = list[i - 1];
      const showDivider = i > 0 && railKeyOf(prev) !== railKeyOf(entry);
      return (
        <Fragment key={ws.id}>
          {grouped && i === 0 && (
            <span
              className="self-center max-w-32 truncate px-1 text-[11px] text-muted-foreground"
              data-tray-group-label={railKeyOf(entry)}
              title={group.title}
            >
              {group.title}
            </span>
          )}
          {showDivider && (
            <span aria-hidden data-tray-divider="" className="my-1 w-px flex-shrink-0 bg-border" />
          )}
          <TrayChip
            entry={entry}
            index={i}
            isActive={ws.id === activeWorkspaceId}
            teamKey={teamKey}
            crew={crews.get(ws.id)}
            compact={compactChips}
            oneLine
            // The repo glyph reads in the pill's own ink: colour on a pill means status or machine.
            glyph={slug ? glyphMap?.get(slug) : undefined}
            liveTerminalCount={
              entry.minimized
                ? (minimizedTabsForTeam?.[ws.id]?.length ?? 0)
                : (terminalTabsForTeam?.[ws.id]?.length ?? 0)
            }
            statusPopoverOpen={statusPopover === ws.id}
            onStatusPopoverOpenChange={(open) => setStatusPopover(open ? ws.id : null)}
            onSelect={pillSelect}
            onClose={(id) => void closeWorkspace(teamKey, id)}
            onContextMenu={(id, minimized, x, y) => {
              setChipMenu({ id, minimized, pos: { x, y } });
              onContextMenu?.(id, minimized, x, y);
            }}
            onStoryStatusChange={onStoryStatusChange}
            onOpenStoryDoc={onOpenStoryDoc ? openChipDoc : undefined}
          />
        </Fragment>
      );
    });

  // Both placements count and badge every workspace scrolled out of view.
  const overflowButton =
    hiddenWorkspaces.length > 0 ? (
      <TrayOverflowButton
        key={teamKey}
        teamKey={teamKey}
        count={hiddenWorkspaces.length}
        noun={["workspace", "workspaces"]}
        statusWorkspaceIds={hiddenWorkspaceIds}
        testId="workspace-tray-overflow"
        menuZoom={topChrome && !grouped ? 1 : barZoom}
      >
        {(close) => (
          <>
            {hiddenWorkspaces.map((w) => (
              <HiddenChipRow
                key={w.id}
                teamKey={teamKey}
                workspace={w}
                // The same path as a click on its pill (the pane's card opens and flashes), at once:
                // a menu row is never the first half of a double-click.
                onSelect={(id) => {
                  close();
                  pillSelect(id, { keyboard: true });
                }}
              />
            ))}
            {/* Offer the pinned launch action from the overflow menu too. */}
            <button
              type="button"
              onClick={() => {
                close();
                onNewWorkspace();
              }}
              className="mt-1 flex w-full items-center gap-1.5 rounded border-t border-border px-2 py-1 text-left text-xs text-muted-foreground hover:bg-accent/40 hover:text-foreground"
            >
              <Plus className="h-3 w-3" />
              New workspace
            </button>
          </>
        )}
      </TrayOverflowButton>
    ) : null;

  // Grouped mode: epic pills scrolled out of the rail get the same "+N".
  const railOverflowButton =
    hiddenRails.length > 0 ? (
      <TrayOverflowButton
        key={teamKey}
        teamKey={teamKey}
        count={hiddenRails.length}
        noun={["epic", "epics"]}
        statusWorkspaceIds={hiddenRailMemberIds}
        testId="workspace-epic-rail-overflow"
        // The rail is in the scaled bar except when portaled into the title bar.
        menuZoom={topChrome ? 1 : barZoom}
      >
        {(close) =>
          hiddenRails.map((r) => (
            <HiddenRailRow
              key={r.railKey}
              teamKey={teamKey}
              rail={r}
              onSelect={(railEntry) => {
                close();
                onSelectEpic(railEntry);
              }}
            />
          ))
        }
      </TrayOverflowButton>
    ) : null;

  // Trailing launcher — always present (even when empty), never draggable.
  const launcher = (
    <button
      type="button"
      data-testid="workspace-tray-launcher"
      aria-label="New workspace"
      className={TRAY_CONTROL_BTN_CLASS}
      onClick={onNewWorkspace}
    >
      <Plus className="h-3.5 w-3.5" />
    </button>
  );

  // SW-EW-38 (AC-2): the Flat/Grouped lens toggle. Lives in the right cluster
  // (the SW-EW-35 controls slot) in BOTH modes so it's always reachable — incl.
  // an empty grouped tray (R6). Reflects the current mode; flips per-team.
  const modeToggleLabel = grouped
    ? "Tray mode: grouped (switch to flat)"
    : "Tray mode: flat (switch to grouped)";
  const modeToggle = (
    // The fixed control slot also fits the epic rail's chrome-height row.
    <div className="flex h-8 max-h-full flex-shrink-0 items-center self-start">
      <Tooltip>
        <TooltipTrigger asChild>
          <button
            type="button"
            data-testid="workspace-tray-mode-toggle"
            aria-label={modeToggleLabel}
            aria-pressed={grouped}
            className={CHROME_ICON_BUTTON}
            onClick={() => {
              closeMenu(); // the chip a menu was opened from relocates between strips
              setTrayMode(teamKey, grouped ? "flat" : "grouped");
            }}
          >
            {grouped ? (
              <Layers {...CHROME_ICON} aria-hidden="true" />
            ) : (
              <Rows3 {...CHROME_ICON} aria-hidden="true" />
            )}
          </button>
        </TooltipTrigger>
        <TooltipContent side={topChrome ? "bottom" : "top"}>
          {grouped ? "Grouped by epic — switch to flat" : "Flat — switch to grouped by epic"}
        </TooltipContent>
      </Tooltip>
    </div>
  );

  // Strip building blocks. Bottom strips carry the SW-EW-33 chrome (top border +
  // muted bg); the strip portaled into the title bar (console) blends in with no
  // border/bg (SW-EW-34) via `blend`.
  // One-row strips scroll sideways without a scrollbar painted over the chips:
  // what scrolls out of view is counted by the strip's "+N" (as the editor tab
  // strip does).
  const SCROLL_AREA =
    "relative flex min-w-0 flex-1 items-stretch gap-1 overflow-x-auto overflow-y-hidden [container-type:inline-size] [container-name:workspace-tray] [scrollbar-width:none] [&::-webkit-scrollbar]:hidden";
  const STRIP_BASE = "flex w-full flex-shrink-0 px-1";
  // Bottom strips keep a 2px pad; the strip portaled into the 40px title bar pads
  // its first row onto the bar's centre line, level with the identity chip and
  // the right-hand controls.
  const stripPad = (blend: boolean) => (blend ? "py-1" : "py-0.5");
  // One-row strips have a 32px floor and grow with their content — never a fixed
  // height, which clipped any chip taller than the padded row (the restored 16px
  // labels, a Large bar).
  const SINGLE_ROW = "min-h-8 items-stretch";
  // The epic rail is a selector of chrome-sized pills, not a chip surface: it
  // keeps its fixed 32px row (its controls fit it via `max-h-full`), so the
  // grouped bar doesn't grow a second full-height row.
  const RAIL_ROW = "h-8 items-stretch";
  const chipStripHeight = SINGLE_ROW;
  const chipStripInner = SCROLL_AREA;
  const BOTTOM_CHROME = "border-t border-border bg-muted/40";
  // SLW-FX-19: when two grouped strips stack, the LOWER strip softens its top
  // border so the divider *between* the strips reads faint, while the UPPER
  // strip keeps the full-strength BOTTOM_CHROME edge against the terminal above.
  const SOFT_DIVIDER = "border-border/60";

  // Pinned alongside the scrolling strip and trailing controls.
  const leadingSlot = leading ? (
    // `empty:hidden`: the Agents control renders nothing while the agents list is
    // on screen, and the slot's divider must not strand alone.
    <div className="mr-1 flex h-8 flex-shrink-0 items-center self-start border-r border-border/60 pr-1 empty:hidden">
      {leading}
    </div>
  ) : null;

  // The epic-rail strip — grouped mode's selector. SLW-FX-19: shared is now the
  // BOTTOM strip (under the story row), so the non-blend variant carries the
  // SOFT_DIVIDER inter-strip divider (the lower strip owns the soft divider; the
  // upper strip owns the full outer edge). Console: portaled into the title-bar
  // slot (`blend`) — border/bg-less, unaffected.
  const railStrip = (blend: boolean) => (
    <div
      role="toolbar"
      aria-label="Epic rail"
      data-testid="workspace-epic-rail"
      className={`${STRIP_BASE} ${blend ? "h-10 items-stretch py-1.5" : `${RAIL_ROW} py-0.5 ${BOTTOM_CHROME} ${SOFT_DIVIDER}`}`}
      // OC-6 (AC3): rail-local roving — ArrowLeft/Right/Home/End move the pill tab
      // stop (handler self-gates to pill focus, so the mode toggle is unaffected).
      onKeyDown={onRailKeyDown}
      // SW-EW-41: when portaled into the title bar (console), this strip is the
      // title bar's window-drag surface. It's foreign-portaled content, so the
      // title-bar row's own onMouseDown never fires for it — attach startDragging
      // here. It skips interactive controls + `[data-tray-chip]`, so empty strip
      // area moves the window while pills stay clickable/draggable.
      onMouseDown={blend ? startDragging : undefined}
    >
      {/* OC-6 (AC4): polite announcement of the selected epic (a keyboard command
          moves focus to the terminal, not the pill, so selection changes are
          otherwise inaudible to assistive tech). */}
      <div aria-live="polite" className="sr-only" data-testid="epic-rail-live">
        {selectedRailTitle
          ? `${railInfo(teamKey, selectedRailKey).kind === "epic" ? "Epic" : "Group"} ${selectedRailTitle} selected`
          : ""}
      </div>
      <div ref={setRailArea} className={SCROLL_AREA}>
        {rail.map((r, i) => (
          <EpicRailPill
            key={r.railKey}
            rail={r}
            selected={r.railKey === selectedRailKey}
            tabIndex={i === activeRailIndex ? 0 : -1}
            buttonRef={(el) => {
              // Delete on unmount so the ref map can't accumulate stale nulls as
              // rails come and go.
              if (el) railButtonRefs.current.set(r.railKey, el);
              else railButtonRefs.current.delete(r.railKey);
            }}
            teamKey={teamKey}
            spineColor={
              railInfo(teamKey, r.railKey).kind === "epic"
                ? getEpicSpineColor(r.railKey, isLightTheme)
                : undefined
            }
            onContextMenu={
              customGroupId(r.railKey) !== null
                ? (x, y) => {
                    setChipMenu(null);
                    setRailMenu({ id: customGroupId(r.railKey)!, pos: { x, y } });
                  }
                : undefined
            }
            onSelect={onSelectEpic}
          />
        ))}
      </div>
      {railOverflowButton}
      {modeToggle}
      {trailing}
    </div>
  );

  // The selected epic's story row. SLW-FX-19: in shared placement this is now
  // the TOP strip (over the rail), so it owns the full-strength outer top edge
  // — pass `soften=false`. In the console placement it's the sole bottom strip
  // under the title-bar rail, keeping the softened divider (`soften=true`).
  const childrenStrip = (soften: boolean) => (
    <div
      role="toolbar"
      aria-label="Workspace tray"
      data-testid="workspace-tray-children"
      className={`${STRIP_BASE} py-0.5 ${chipStripHeight} ${BOTTOM_CHROME}${soften ? ` ${SOFT_DIVIDER}` : ""}`}
    >
      {leadingSlot}
      <div
        ref={setChipArea}
        className={chipStripInner}
        data-scroll-edges={chipEdges}
        style={chipAreaStyle}
        onFocus={onChipAreaFocus}
        onKeyDown={onChipAreaKeyDown}
      >
        {renderChips(children)}
        {insertionLine}
      </div>
      {overflowButton}
      {launcher}
    </div>
  );

  // The single-row flat strip (flat mode, either placement).
  const flatStrip = (blend: boolean) => (
    <div
      role="toolbar"
      aria-label="Workspace tray"
      data-testid="workspace-tray"
      className={`${STRIP_BASE} ${stripPad(blend)} ${chipStripHeight} ${blend ? "" : BOTTOM_CHROME}`}
      // SW-EW-41: see railStrip — in console the portaled strip is the title
      // bar's drag surface; startDragging skips controls + `[data-tray-chip]`.
      onMouseDown={blend ? startDragging : undefined}
    >
      {leadingSlot}
      <div
        ref={setChipArea}
        className={chipStripInner}
        data-scroll-edges={chipEdges}
        style={chipAreaStyle}
        onFocus={onChipAreaFocus}
        onKeyDown={onChipAreaKeyDown}
      >
        {renderChips(entries)}
        {insertionLine}
      </div>
      {overflowButton}
      {launcher}
      {/* SW-EW-38 mode toggle, then SW-EW-35 right-pinned CLI controls (trailing
          self-gates; undefined in console where the controls live in the title
          bar). The controls stay pinned beside the scrolling chip area. */}
      {modeToggle}
      {trailing && <div className="flex h-8 items-center self-start">{trailing}</div>}
    </div>
  );

  // AC-8: one-time, dismissible coachmark above the bottom tray on first run.
  const coachmark =
    !seenCoachmark && entries.length > 0 ? (
      <div
        data-testid="workspace-tray-coachmark"
        className="absolute bottom-full left-1/2 z-10 mb-1 flex -translate-x-1/2 items-center gap-2 whitespace-nowrap rounded border border-border bg-popover px-2 py-1 text-xs text-muted-foreground shadow-md"
      >
        <span>
          Click to switch · drag along the bar (or {isMac() ? "⌥←/→" : "Alt+←/→"}) to reorder, onto
          a pane to split
        </span>
        <button
          type="button"
          aria-label="Dismiss"
          className="text-muted-foreground hover:text-foreground"
          onClick={markCoachmarkSeen}
        >
          <X className="h-3 w-3" />
        </button>
      </div>
    ) : null;

  const chipHiddenTabs = useHiddenTerminalTabs(teamKey, chipMenu?.id ?? "");
  // A root workspace is never hidden or ended: its menu only restores its Hidden tabs, so once
  // the last of them is restored (here or anywhere) it has nothing left to offer and closes.
  const menuOnRoot = workspaces.find((w) => w.id === chipMenu?.id)?.kind === "root";
  const rootMenuEmpty = !!chipMenu && menuOnRoot && chipHiddenTabs.length === 0;
  // Architecture: closing an emptied native menu also settles its host request.
  useEffect(() => {
    // Settle the external host after render, along with this component's menu state.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    if (rootMenuEmpty) closeMenu();
  });
  // "Remove worktree…" is offered when the chip's workspace sits in a managed worktree.
  const chipWorkspace = chipMenu
    ? (workspaces.find((w) => w.id === chipMenu.id) ??
      softClosedForTeam?.[chipMenu.id]?.workspace ??
      null)
    : null;
  const chipRemoveTarget = chipWorkspace
    ? removeWorktreeTargetFor(teamKey, chipWorkspace, worktreeRows)
    : null;
  useEffect(() =>
    registerNativeMenu(teamKey, "workspace", (request) => {
      const pos = { x: request.anchorRect.x, y: request.anchorRect.y };
      if (request.row.kind === "group") {
        if (!customGroups?.some((group) => group.id === request.row.groupId)) return null;
        closeMenu();
        setRailMenu({ id: request.row.groupId as string, pos, owner: request.owner });
      } else {
        const workspace =
          workspaces.find((w) => w.id === request.row.workspaceId) ??
          softClosedForTeam?.[String(request.row.workspaceId)]?.workspace;
        if (!workspace) return null;
        if (
          workspace.kind === "root" &&
          !selectHiddenTerminalTabs(useViewerStore.getState(), teamKey, workspace.id).length
        )
          return null;
        closeMenu();
        setChipMenu({
          id: workspace.id,
          minimized: !!softClosedForTeam?.[workspace.id],
          pos,
          owner: request.owner,
        });
      }
      externalMenu.current = { owner: request.owner, close: request.onClose };
      return () => {
        if (externalMenu.current?.owner !== request.owner) return;
        externalMenu.current = null;
        setChipMenu((current) => (current?.owner === request.owner ? null : current));
        setRailMenu((current) => (current?.owner === request.owner ? null : current));
      };
    }),
  );
  useEffect(() => () => externalMenu.current?.close(), []);
  const contextMenu = chipMenu?.pos ? (
    <WorkspaceGroupMenu
      key={`${teamKey}:${chipMenu.id}`}
      owner={chipMenu.owner}
      position={chipMenu.pos}
      onClose={closeMenu}
      teamKey={teamKey}
      workspace={chipWorkspace}
      footer={
        chipRemoveTarget && (
          <>
            <div role="separator" className="my-1 border-t border-border" />
            <PositionedMenuItem
              type="button"
              data-testid="workspace-remove-worktree"
              onClick={() => {
                setRemoveTarget(chipRemoveTarget);
                closeMenu();
              }}
            >
              Remove worktree…
            </PositionedMenuItem>
          </>
        )
      }
    >
      {chipWorkspace?.kind === "context" && (
        <PositionedMenuItem
          type="button"
          onClick={() => {
            openLaunchSheet({
              teamKey,
              contextWorktreeId: chipWorkspace.contextWorktreeId ?? chipWorkspace.id,
            });
            closeMenu();
          }}
        >
          Launch agent here
        </PositionedMenuItem>
      )}
      {chipHiddenTabs.length > 0 && (
        <>
          <div role="group" aria-label="Hidden tabs">
            <div
              aria-hidden="true"
              className="px-2 py-1 text-[11px] uppercase tracking-wide text-muted-foreground"
            >
              Hidden tabs
            </div>
            {chipHiddenTabs.map((t) => (
              <HiddenTabMenuItems
                key={t.id}
                teamKey={teamKey}
                workspaceId={chipMenu.id}
                tab={t}
                onDone={closeMenu}
              />
            ))}
          </div>
          {!menuOnRoot && <div role="separator" className="my-1 border-t border-border" />}
        </>
      )}
      {menuOnRoot ? null : chipMenu.minimized ? (
        <PositionedMenuItem
          type="button"
          onClick={() => {
            useViewerStore.getState().reopenSoftClosedWorkspace(teamKey, chipMenu.id);
            closeMenu();
          }}
        >
          Restore
        </PositionedMenuItem>
      ) : (
        // Hide into the tray, still running (an agent chip hides back to headless).
        <PositionedMenuItem
          type="button"
          onClick={() => {
            hideWorkspace(teamKey, chipMenu.id);
            closeMenu();
          }}
        >
          Hide
        </PositionedMenuItem>
      )}
      {/* End kills its terminals — the in-app confirmation asks first when
          anything is still running (live or hidden in the tray). */}
      {!menuOnRoot && (
        <PositionedMenuItem
          type="button"
          onClick={() => {
            void endWorkspace(teamKey, chipMenu.id);
            closeMenu();
          }}
        >
          End
        </PositionedMenuItem>
      )}
    </WorkspaceGroupMenu>
  ) : railMenu ? (
    <WorkspaceGroupMenu
      key={`${teamKey}:${railMenu.id}`}
      owner={railMenu.owner}
      position={railMenu.pos}
      onClose={closeMenu}
      teamKey={teamKey}
      groupId={railMenu.id}
    />
  ) : null;
  const removeDialog = (
    <RemoveWorktreeDialog target={removeTarget} onClose={() => setRemoveTarget(null)} />
  );

  // The bottom bar's box. `flex-shrink-0`: it sits last in the left column's
  // flex column (overflow hidden); were it allowed to shrink, a squeeze from the
  // panes above would make the box shorter than its strips and push their bottom
  // edge — pills and all — under the window's bottom edge.
  const BAR_BOX = "relative flex-shrink-0";
  // The strips, drawn at the "Workspace bar size" zoom. The chip menu and the
  // coachmark stay outside it: the menu is `position: fixed` at the pointer's
  // viewport coordinates, which a zoomed ancestor would scale off the pointer.
  const scaledBar = (strips: React.ReactNode) => (
    <div
      data-bar-size={barSize}
      // `--bar-zoom` lets a part keep a floor (badge text, the 24px hit target) at Small.
      style={{ zoom: barZoom, ["--bar-zoom" as string]: barZoom }}
    >
      {strips}
    </div>
  );

  // SW-EW-34/38 — CONSOLE placement: the top part portals into the title-bar
  // slot. Flat → the whole switcher; grouped → the epic rail up top + the
  // selected epic's story row inline at the bottom (the split Trevor asked for).
  if (topChrome) {
    // The active team owns the single slot; until it's registered (or for a
    // backgrounded team) render nothing rather than stack a second switcher.
    if (!consoleRailSlot) return null;
    return (
      <>
        {createPortal(grouped ? railStrip(true) : flatStrip(true), consoleRailSlot)}
        <TrayMoveAnnouncer key={teamKey} teamKey={teamKey} entries={entries} />
        {grouped && (
          <div ref={toastClearanceRef} className={BAR_BOX} data-workspace-bar="">
            {coachmark}
            {scaledBar(childrenStrip(true))}
          </div>
        )}
        {contextMenu}
        {removeDialog}
      </>
    );
  }

  // SHARED placement: everything inline at the bottom. Grouped = two stacked
  // rows. SLW-FX-19: story chips on TOP, epic-selector rail on the BOTTOM
  // (nearest the CLI chrome) — DOM order == visual top→bottom in `flex-col`.
  return (
    <div ref={toastClearanceRef} className={BAR_BOX} data-workspace-bar="">
      {coachmark}
      <TrayMoveAnnouncer key={teamKey} teamKey={teamKey} entries={entries} />
      {scaledBar(
        grouped ? (
          <div
            data-testid="workspace-tray"
            data-tray-mode="grouped"
            className="flex w-full flex-col"
          >
            {childrenStrip(false)}
            {railStrip(false)}
          </div>
        ) : (
          flatStrip(false)
        ),
      )}
      {contextMenu}
      {removeDialog}
    </div>
  );
}
