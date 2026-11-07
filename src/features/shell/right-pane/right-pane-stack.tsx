// SPDX-License-Identifier: MIT
/**
 * SW-RP-1: the stacked-sections renderer for the right pane.
 *
 * Renders `rightPaneLayout` as vertically stacked, resizable sections, each a
 * tab-group of registered panels. Two invariants the model can't enforce alone:
 *  - **Mount preservation:** every panel that has been shown stays MOUNTED and is
 *    hidden via `display:none` when collapsed or on an inactive tab — never
 *    conditionally unmounted — so react-flow viewport / Tiptap editor state
 *    survives collapse + tab switches (the #1 regression this story guards).
 *    Panels mount lazily on first becoming visible (avoids react-flow #004 from
 *    mounting into a zero-height collapsed section), then stick.
 *  - **Collapse floor:** the collapse control on the last expanded section is
 *    disabled (the model no-ops it too); hiding everything is the pane toggle.
 */
import {
  DndContext,
  DragOverlay,
  PointerSensor,
  closestCenter,
  pointerWithin,
  useDroppable,
  useSensor,
  useSensors,
  type CollisionDetection,
  type DragEndEvent,
  type DragStartEvent,
} from "@dnd-kit/core";
import { SortableContext, useSortable, verticalListSortingStrategy } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { ChevronDown, ChevronRight, GripVertical } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { computeClampedRatio } from "@/features/viewer/views/drag-resize-utils";
import { useDragResize } from "@/features/viewer/views/use-drag-resize";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { PanelTabStrip, type PanelTab } from "./panel-tab-strip";
import { usePanelsFor, type PanelDefinition } from "./panel-registry";
import { useOwningScope } from "@/shared/lib/warm-scopes";
import {
  applyRightPaneDrag,
  gapDropId,
  parseGapIndex,
  parseSectionId,
  parseTabId,
  resolveRightPaneDrag,
  sectionDragId,
} from "./right-pane-dnd";
import { useRightPaneHost } from "./right-pane-host-context";
import {
  collapseSection,
  expandSection,
  expandedSectionCount,
  resizePairAt,
  selectRenderableLayout,
  setActivePanel,
  setSectionSize,
  type RightPaneSection,
} from "./right-pane-layout";
import { usePanelDragSource } from "./use-panel-drag-source";
import { useReparentingPanelBodies } from "./use-reparenting-panel-bodies";
import { PanelMaximizeButton } from "./panel-maximize-boundary";

type ElRefs = React.MutableRefObject<Map<string, HTMLDivElement>>;

const sizeVar = (id: string) => `--rps-size-${id}`;

/** Route collisions by what is being dragged so each gesture hits the right kind
 *  of target reliably (the thin tab/gap droppables would otherwise lose to the
 *  much taller section bodies under a naive `closestCenter`):
 *   - **section drag** → closest section droppable (sortable wants `closestCenter`
 *     for a smooth shuffle even between sections).
 *   - **tab drag** → STRICTLY pointer-within: a sibling/foreign tab (reorder/group),
 *     else a gap (ungroup), else the section the pointer is inside (group). Released
 *     over empty space (outside every target) → no `over` → a no-op, so a tab drag
 *     can be abandoned without an accidental regroup. */
const rightPaneCollision: CollisionDetection = (args) => {
  const activeId = args.active.id;
  if (parseSectionId(activeId) != null) {
    return closestCenter({
      ...args,
      droppableContainers: args.droppableContainers.filter((c) => parseSectionId(c.id) != null),
    });
  }

  const hits = pointerWithin(args);
  const tab = hits.find((c) => parseTabId(c.id) != null && c.id !== activeId);
  if (tab) return [tab];
  const gap = hits.find((c) => parseGapIndex(c.id) != null);
  if (gap) return [gap];
  const section = hits.find((c) => parseSectionId(c.id) != null);
  return section ? [section] : [];
};

/** What is currently being dragged — drives the gap drop-zones (shown only for a
 *  tab drag → ungroup) and the DragOverlay ghost label. */
interface ActiveDrag {
  kind: "section" | "tab";
  label: string;
}

export function RightPaneStack() {
  const layout = useViewerStore((s) => s.rightPaneLayout);
  const setLayout = useViewerStore((s) => s.setRightPaneLayout);
  const panels = usePanelsFor(useOwningScope());
  const host = useRightPaneHost();

  // A section may reference a panel that isn't registered in the current context
  // (a custom pane another team owns, or one still mid-async-registration). Render
  // off a filtered PROJECTION — dead sections dropped, hidden-active fallback
  // applied — while every WRITE below stays on the raw `layout`, so an absent panel
  // is hidden, never removed (absent ≠ closed). Recomputes when the registry
  // changes, so a panel appears the moment its definition lands.
  const renderLayout = useMemo(
    () => selectRenderableLayout(layout, (id) => panels[id] != null),
    [layout, panels],
  );
  const closeShortcut = formatKeystrokeDisplay(useEffectiveShortcut("viewer.toggle-right-pane"));

  // Distance-gated pointer sensor so a tap (collapse toggle / tab select) is not
  // swallowed by drag activation — mirrors the terminal/tray DndContext.
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 6 } }));
  const [activeDrag, setActiveDrag] = useState<ActiveDrag | null>(null);

  const labelForPanel = (panelId: string) => panels[panelId]?.title ?? panelId;
  const labelForSection = (section: RightPaneSection) => labelForPanel(section.group.activePanelId);

  const handleDragStart = (event: DragStartEvent) => {
    const sectionId = parseSectionId(event.active.id);
    if (sectionId != null) {
      const section = layout.sections.find((s) => s.id === sectionId);
      setActiveDrag({ kind: "section", label: section ? labelForSection(section) : sectionId });
      return;
    }
    const panelId = parseTabId(event.active.id);
    if (panelId != null) setActiveDrag({ kind: "tab", label: labelForPanel(panelId) });
  };

  const handleDragEnd = (event: DragEndEvent) => {
    setActiveDrag(null);
    const op = resolveRightPaneDrag(layout, event.active.id, event.over?.id);
    // Single synchronous, non-blocking commit (set → debounced persist).
    if (op) setLayout(applyRightPaneDrag(layout, op));
  };

  const elRefs: ElRefs = useRef(new Map());
  // Mount preservation across reorder / group / ungroup / collapse / tab-switch:
  // each panel body lives in a stable reparented container, never remounted as it
  // moves between sections (AC-5). See the hook for the #004-safe lazy gate.
  const { registerSlot, portals } = useReparentingPanelBodies(renderLayout, panels);

  const expandedCount = expandedSectionCount(renderLayout);
  // Raw total weight of expanded sections — used to NORMALIZE each section's
  // flex-grow so the grows sum to 1.0 and fully fill the pane. (Flexbox only
  // distributes the WHOLE free space when grows sum to ≥1; a lone expanded
  // section at grow 0.55 would otherwise leave a 45% gap.)
  const rawExpandedTotal = renderLayout.sections
    .filter((s) => !s.collapsed)
    .reduce((sum, s) => sum + (s.size || 0), 0);
  const totalExpandedSize = rawExpandedTotal || 1; // divisor guard for the divider
  // Per-section normalized grow. If all expanded sizes are degenerate (sum 0 —
  // only reachable via corrupted/MCP layout, since coerce accepts size 0), fall
  // back to EQUAL shares so the pane still fills instead of leaving a gap.
  const growFractionFor = (s: RightPaneSection): number =>
    rawExpandedTotal > 0 ? (s.size || 0) / rawExpandedTotal : 1 / Math.max(1, expandedCount);

  const toggleCollapse = (section: RightPaneSection) => {
    setLayout(
      section.collapsed ? expandSection(layout, section.id) : collapseSection(layout, section.id),
    );
  };

  // Every panel closed (all unchecked in View → Panels). Render a guiding
  // empty-state rather than a blank pane, so reopening with the toggle shortcut
  // isn't a dead-end — the user re-enables a panel from the menu (or closes).
  if (renderLayout.sections.length === 0) {
    return (
      <div
        className="flex h-full flex-col items-center justify-center gap-2 border-l px-6 text-center"
        style={{ borderColor: "var(--cs-sidebar-border, var(--border))" }}
      >
        <p className="text-sm text-muted-foreground">No panels open</p>
        <p className="text-xs text-muted-foreground/70">
          Turn a panel on from View → Panels, or press {closeShortcut} to close this pane.
        </p>
        <button
          type="button"
          onClick={host.onClose}
          className="text-xs text-muted-foreground hover:text-foreground"
        >
          Close pane
        </button>
      </div>
    );
  }

  // Gap drop-zones (ungroup targets) are rendered only while a TAB is dragged, so
  // they never compete with the section sortable's collision during a reorder.
  const showGaps = activeDrag?.kind === "tab";

  return (
    <DndContext
      sensors={sensors}
      collisionDetection={rightPaneCollision}
      onDragStart={handleDragStart}
      onDragEnd={handleDragEnd}
      onDragCancel={() => setActiveDrag(null)}
    >
      <SortableContext
        items={renderLayout.sections.map((s) => sectionDragId(s.id))}
        strategy={verticalListSortingStrategy}
      >
        <div
          data-right-pane-root
          className="flex flex-col h-full border-l"
          style={{ borderColor: "var(--cs-sidebar-border, var(--border))" }}
        >
          {/* INVARIANT: `section` here is a RENDER projection — its `group.panelIds`
              may have an absent panel filtered out. Write handlers below address the
              raw `layout` by `section.id` ONLY; never write a projected section (or
              its filtered `panelIds`) back, or a hidden panel would be deleted from
              storage (absent → closed). */}
          {renderLayout.sections.map((section, i) => {
            const pair = resizePairAt(renderLayout.sections, i);
            return (
              <SectionView
                key={section.id}
                section={section}
                panels={panels}
                repoPath={host.repoPath}
                elRefs={elRefs}
                registerSlot={registerSlot}
                isTabDropTarget={showGaps}
                growFraction={growFractionFor(section)}
                canCollapse={section.collapsed || expandedCount > 1}
                onToggleCollapse={() => toggleCollapse(section)}
                onSelectPanel={(panelId) => setLayout(setActivePanel(layout, section.id, panelId))}
                gapBefore={showGaps ? <GapDropZone index={i} /> : null}
                gapAfter={
                  showGaps && i === renderLayout.sections.length - 1 ? (
                    <GapDropZone index={i + 1} />
                  ) : null
                }
                divider={
                  pair ? (
                    <SectionResizeDivider
                      aboveId={pair.above.id}
                      belowId={pair.below.id}
                      aboveSize={pair.above.size}
                      belowSize={pair.below.size}
                      totalExpandedSize={totalExpandedSize}
                      elRefs={elRefs}
                      onCommit={(aboveSize, belowSize) =>
                        setLayout(
                          setSectionSize(
                            setSectionSize(layout, pair.above.id, aboveSize),
                            pair.below.id,
                            belowSize,
                          ),
                        )
                      }
                    />
                  ) : null
                }
              />
            );
          })}
        </div>
      </SortableContext>
      {/* Panel bodies render here once, into stable reparented containers (kept
          mounted across every rearrange — AC-5). They portal into each section's
          body slot; their position in this React tree is irrelevant. */}
      {portals}
      <DragOverlay dropAnimation={null}>
        {activeDrag ? (
          <div
            className="flex items-center gap-1 rounded border px-2 py-1 text-xs font-medium shadow-md"
            style={{
              backgroundColor: "var(--cs-viewer-bg, var(--background))",
              borderColor: "var(--border)",
              color: "var(--foreground)",
            }}
          >
            {activeDrag.label}
          </div>
        ) : null}
      </DragOverlay>
    </DndContext>
  );
}

/** A thin droppable band between sections — the ungroup target. Rendered only
 *  during a tab drag (see `showGaps`); highlights when the tab is over it. */
function GapDropZone({ index }: { index: number }) {
  const { setNodeRef, isOver } = useDroppable({ id: gapDropId(index) });
  return (
    <div
      ref={setNodeRef}
      data-testid={`rps-gap-${index}`}
      className="flex-shrink-0 transition-colors"
      style={{
        height: isOver ? "20px" : "12px",
        backgroundColor: isOver ? "color-mix(in srgb, var(--primary) 25%, transparent)" : undefined,
      }}
    />
  );
}

interface SectionViewProps {
  section: RightPaneSection;
  panels: Record<string, PanelDefinition>;
  /** The host view's source-repo path — threaded to the generic maximize button. */
  repoPath: string;
  elRefs: ElRefs;
  /** Register this section's body slot DOM node so panel bodies reparent into it. */
  registerSlot: (sectionId: string, el: HTMLDivElement | null) => void;
  /** A tab is being dragged → this section is a potential group target; show the
   *  drop affordance when hovered. */
  isTabDropTarget: boolean;
  /** Normalized flex-grow (section.size / total expanded size) so expanded
   *  sections always sum to 1.0 and fill the pane. */
  growFraction: number;
  canCollapse: boolean;
  onToggleCollapse: () => void;
  onSelectPanel: (panelId: string) => void;
  divider: React.ReactNode;
  gapBefore: React.ReactNode;
  gapAfter: React.ReactNode;
}

function SectionView({
  section,
  panels,
  repoPath,
  elRefs,
  registerSlot,
  isTabDropTarget,
  growFraction,
  canCollapse,
  onToggleCollapse,
  onSelectPanel,
  divider,
  gapBefore,
  gapAfter,
}: SectionViewProps) {
  const { collapsed, group } = section;
  const registeredTabs: PanelTab[] = group.panelIds
    .map((id) => panels[id])
    .filter((p): p is PanelDefinition => p != null)
    .map((p) => ({ id: p.id, title: p.title, icon: p.icon, badge: p.badge }));
  const activeDef = panels[group.activePanelId];

  // The section is a sortable item: its body/header is the reorder draggable +
  // the group drop target (a tab dropped onto it → merge). Listeners go on the
  // grip handle only (so the header click still toggles collapse). `isOver` lights
  // the group drop affordance while a tab is dragged over it (AC-4).
  const {
    setNodeRef: setSortableRef,
    setActivatorNodeRef,
    listeners,
    attributes,
    transform,
    transition,
    isDragging,
    isOver,
  } = useSortable({ id: sectionDragId(section.id) });
  const groupTargetHighlight = isTabDropTarget && isOver;

  return (
    <>
      {gapBefore}
      <div
        ref={(el) => {
          setSortableRef(el);
          if (el) elRefs.current.set(section.id, el);
          else elRefs.current.delete(section.id);
        }}
        className="flex flex-col overflow-hidden"
        style={{
          // Normalized flex-grow (via CSS var for drag visual-follow) + basis 0
          // so expanded sections fill the pane; collapsed = title-bar height.
          flexGrow: collapsed ? 0 : `var(${sizeVar(section.id)}, ${growFraction})`,
          flexShrink: collapsed ? 0 : 1,
          flexBasis: collapsed ? "auto" : "0%",
          minHeight: collapsed ? undefined : "32px",
          backgroundColor: "var(--cs-viewer-bg, var(--background))",
          // dnd-kit reorder visual-follow (does NOT remount the node → panel
          // state survives the reorder).
          transform: CSS.Transform.toString(transform),
          transition,
          opacity: isDragging ? 0.5 : undefined,
          zIndex: isDragging ? 20 : undefined,
          // Group drop affordance: ring the section when a tab is dragged over it.
          boxShadow: groupTargetHighlight
            ? "inset 0 0 0 2px color-mix(in srgb, var(--primary) 60%, transparent)"
            : undefined,
        }}
      >
        {/* Section title bar — click toggles collapse/expand */}
        {/* SW-WG-20 (AC-2): the STABLE row a header-action set measures for its width-driven
            inline cap. Its width is invariant to how many actions render (the title yields
            first via `truncate min-w-0`), so observing it can never feed back into itself. */}
        <div
          data-panel-header-row
          className={`flex h-8 items-center justify-between gap-1 px-2 border-b flex-shrink-0 select-none ${canCollapse ? "cursor-pointer" : ""}`}
          style={{
            backgroundColor: "color-mix(in srgb, var(--card) 50%, transparent)",
            borderColor: "var(--border)",
          }}
          onClick={() => {
            if (canCollapse) onToggleCollapse();
          }}
        >
          <button
            type="button"
            ref={setActivatorNodeRef}
            {...attributes}
            {...listeners}
            onClick={(e) => e.stopPropagation()}
            aria-label="Drag to reorder section"
            className="text-muted-foreground/60 hover:text-foreground transition-colors cursor-grab touch-none flex-shrink-0"
          >
            <GripVertical size={13} />
          </button>
          {/* SW-AV-10: the passive GROUP label (SETUP / LIVE) — a quiet uppercase
              cue that names the two-group IA. Not a control: it never navigates and
              is aria-hidden (the tabs are the accessible nav). */}
          {section.label ? (
            <span
              aria-hidden="true"
              data-testid={`rps-group-label-${section.id}`}
              className="mr-1 flex-shrink-0 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground/70"
            >
              {section.label}
            </span>
          ) : null}
          {registeredTabs.length > 1 ? (
            <PanelTabStrip
              tabs={registeredTabs}
              activeId={group.activePanelId}
              onSelect={onSelectPanel}
            />
          ) : activeDef ? (
            <DraggableSectionTitle
              title={activeDef.title}
              icon={activeDef.icon}
              panelId={activeDef.id}
              badge={activeDef.badge}
            />
          ) : (
            <div className="flex items-center gap-1 text-xs text-foreground font-medium">
              <span>{group.activePanelId}</span>
            </div>
          )}
          <div className="flex items-center gap-1 ml-auto">
            {/* The section chrome carries ONLY expand + close. A panel's own controls render in a
                toolbar at the top of its BODY (see useReparentingPanelBodies) — never here — so a
                tab strip stays clean when multiple panels share a section. */}
            {/* SW-WG-9: the single generic maximize control for the active, expanded
                panel that declares `maximizable` — replaces the four per-panel buttons. */}
            {!collapsed && activeDef?.maximizable ? (
              <PanelMaximizeButton key={activeDef.id} panelId={activeDef.id} repoPath={repoPath} />
            ) : null}
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                if (canCollapse) onToggleCollapse();
              }}
              disabled={!canCollapse}
              aria-label={collapsed ? "Expand section" : "Collapse section"}
              className={`text-muted-foreground transition-colors px-0.5 ${canCollapse ? "hover:text-foreground" : "opacity-40 cursor-default"}`}
            >
              {collapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
            </button>
          </div>
        </div>

        {/* Body slot: panel bodies are reparented in here (see
            useReparentingPanelBodies) and kept mounted across every rearrange.
            Hidden via display:none when collapsed so the panel state survives. */}
        <div
          ref={(el) => registerSlot(section.id, el)}
          className="flex-col flex-1 min-h-0 overflow-hidden"
          style={{ display: collapsed ? "none" : "flex" }}
        />
      </div>
      {divider}
      {gapAfter}
    </>
  );
}

/** A single-panel section's title, doubling as the `tab:{panelId}` drag source so
 *  a lone panel can be dragged onto another section to group (AC-2). Click still
 *  bubbles to the header's collapse toggle; a 6px drag moves the panel. Rendered
 *  only for single-panel sections, so its drag id never collides with a
 *  `PanelTabStrip` tab. */
function DraggableSectionTitle({
  title,
  icon: Icon,
  panelId,
  badge,
}: {
  title: string;
  icon: PanelDefinition["icon"];
  panelId: string;
  badge?: PanelDefinition["badge"];
}) {
  const { setNodeRef, dragHandleProps, isDragging } = usePanelDragSource(panelId);
  return (
    <div
      ref={setNodeRef}
      {...dragHandleProps}
      className={`flex items-center gap-1 text-xs text-foreground font-medium cursor-grab touch-none ${isDragging ? "opacity-50" : ""}`}
    >
      <Icon size={12} />
      <span>{title}</span>
      {badge?.()}
    </div>
  );
}

/** The smallest height (px) a section may be dragged to — its header plus a
 *  usable sliver of body. Both sides of a splitter honour it. */
const MIN_SECTION_PX = 80;
/** Keyboard nudge per ↑/↓ press (Shift = coarse). */
const KEY_STEP_PX = 16;
const KEY_STEP_COARSE_PX = 64;
/** Two taps on a splitter within this window equalize the pair. */
const DOUBLE_TAP_MS = 400;

/** Clamp a "below share" ratio so both sections keep `MIN_SECTION_PX`. */
function clampPairRatio(ratio: number, containerPx: number): number {
  if (containerPx <= 0) return ratio;
  const min = MIN_SECTION_PX / containerPx;
  if (min >= 1 - min) return 0.5;
  return Math.max(min, Math.min(ratio, 1 - min));
}

interface SectionResizeDividerProps {
  aboveId: string;
  belowId: string;
  aboveSize: number;
  belowSize: number;
  /** Total weight of all expanded sections — the CSS var holds NORMALIZED grow
   *  (weight / total) to match the section's flex-grow units. */
  totalExpandedSize: number;
  elRefs: ElRefs;
  onCommit: (aboveSize: number, belowSize: number) => void;
}

/**
 * The horizontal splitter between two expanded sections.
 *  - Drag: visual-follow via CSS vars (rAF-coalesced in `useDragResize`), one
 *    store commit on release, so dragging never re-renders React per frame.
 *  - Over panes: a pane's HTML lives in an iframe, which swallows pointer events
 *    the moment the cursor crosses it (before the drag threshold engages pointer
 *    capture). A full-window transparent shield is mounted on pointer-down and
 *    held until release, so every move/up reaches this document no matter what
 *    is underneath.
 *  - Double-click: equalizes the pair. ↑/↓ (Shift = coarse) nudge when focused.
 */
function SectionResizeDivider({
  aboveId,
  belowId,
  aboveSize,
  belowSize,
  totalExpandedSize,
  elRefs,
  onCommit,
}: SectionResizeDividerProps) {
  const sum = aboveSize + belowSize;
  // `totalExpandedSize` is already floored to ≥1 at the source (rawExpandedTotal || 1).
  const norm = totalExpandedSize;
  const belowRatio = sum > 0 ? belowSize / sum : 0.5;
  const [pressed, setPressed] = useState(false);
  const lastTapRef = useRef(0);

  const pairHeight = () => {
    const a = elRefs.current.get(aboveId)?.getBoundingClientRect().height ?? 0;
    const b = elRefs.current.get(belowId)?.getBoundingClientRect().height ?? 0;
    return a + b;
  };
  const commitRatio = (r: number) => onCommit((1 - r) * sum, r * sum);

  const { cursor, onPointerDown, ...bindings } = useDragResize({
    // SLW-FX-9 / SW-FX-74: one `dragCursor` carries both the window-wide drag
    // cursor (cleared on every teardown path) and the armed at-rest hover cursor.
    dragCursor: "row-resize",
    getContainerSize: pairHeight,
    getStartRatio: () => belowRatio,
    getPos: (e) => e.clientY,
    computeRatio: ({ startRatio, startPos, currentPos, containerSize }) =>
      computeClampedRatio(startRatio, startPos, currentPos, containerSize, MIN_SECTION_PX, true),
    onVisualUpdate: (r) => {
      // Write NORMALIZED grow (weight / total expanded) so the dragged pair +
      // the other expanded sections' grows still sum to 1.0 → no gap mid-drag.
      elRefs.current
        .get(aboveId)
        ?.style.setProperty(sizeVar(aboveId), String(((1 - r) * sum) / norm));
      elRefs.current.get(belowId)?.style.setProperty(sizeVar(belowId), String((r * sum) / norm));
    },
    onCommit: commitRatio,
    onCleanup: () => {
      elRefs.current.get(aboveId)?.style.removeProperty(sizeVar(aboveId));
      elRefs.current.get(belowId)?.style.removeProperty(sizeVar(belowId));
    },
    onTap: () => {
      const now = performance.now();
      if (now - lastTapRef.current < DOUBLE_TAP_MS) {
        lastTapRef.current = 0;
        if (sum > 0 && belowRatio !== 0.5) commitRatio(0.5);
        return;
      }
      lastTapRef.current = now;
    },
  });

  // Drop the shield on ANY release, wherever it lands. Registered only while
  // pressed; the drag hook's own document listeners do the actual commit.
  useEffect(() => {
    if (!pressed) return;
    const release = () => setPressed(false);
    window.addEventListener("pointerup", release);
    window.addEventListener("pointercancel", release);
    window.addEventListener("blur", release);
    return () => {
      window.removeEventListener("pointerup", release);
      window.removeEventListener("pointercancel", release);
      window.removeEventListener("blur", release);
    };
  }, [pressed]);

  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "ArrowUp" && e.key !== "ArrowDown") return;
    e.preventDefault();
    const px = pairHeight();
    if (px <= 0 || sum <= 0) return;
    const step = (e.shiftKey ? KEY_STEP_COARSE_PX : KEY_STEP_PX) / px;
    // ↑ moves the splitter up: the section above shrinks, the one below grows.
    const next = clampPairRatio(belowRatio + (e.key === "ArrowUp" ? step : -step), px);
    if (next !== belowRatio) commitRatio(next);
  };

  return (
    <div
      data-testid="rps-section-divider"
      data-active={pressed || undefined}
      role="separator"
      aria-orientation="horizontal"
      aria-label="Resize sections"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round((1 - belowRatio) * 100)}
      tabIndex={0}
      title="Drag to resize · double-click to split evenly"
      className="rps-splitter"
      style={{ cursor }}
      onPointerDown={(e) => {
        onPointerDown(e);
        if (e.button === 0) setPressed(true);
      }}
      onKeyDown={onKeyDown}
      {...bindings}
    >
      <span className="rps-splitter-line" aria-hidden="true" />
      <span className="rps-splitter-grip" aria-hidden="true" />
      {pressed
        ? createPortal(
            <div data-testid="rps-resize-shield" className="rps-resize-shield" />,
            document.body,
          )
        : null}
    </div>
  );
}
