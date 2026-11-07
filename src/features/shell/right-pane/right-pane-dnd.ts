// SPDX-License-Identifier: MIT
/**
 * SW-RP-2: the pure drag-resolution glue for the right pane.
 *
 * Mirrors `viewer/lib/terminal-drag.ts`: a small **id grammar** + a pure
 * `(layout, activeId, overId) → drag-op | null` resolver, decoupled from the
 * `DndContext` owner and the draggable/droppable leaves so the three can't
 * disagree on ids or drop semantics — and so the whole drag→op mapping is
 * unit-testable with zero DOM. The resolver only DECIDES; `applyRightPaneDrag`
 * runs the matching SW-RP-1 model op.
 *
 * Three gestures, one id space:
 *  - `section:{id}` — a section drag handle (reorder source) + a droppable group
 *    target when a tab is dropped onto it.
 *  - `tab:{panelId}` — a tab drag (group / ungroup source).
 *  - `gap:{index}`   — a drop band between/below sections (ungroup target; also
 *    a defensive reorder target).
 */
import {
  expandSection,
  findPanelLocation,
  mergePanelIntoSection,
  movePanelWithinSection,
  moveSection,
  splitPanelToOwnSection,
  type RightPaneLayout,
} from "./right-pane-layout";

type DndId = string | number | null | undefined;

const SECTION_PREFIX = "section:";
const TAB_PREFIX = "tab:";
const GAP_PREFIX = "gap:";

/** dnd-kit draggable/droppable id for a section (the reorder handle + group target). */
export const sectionDragId = (sectionId: string): string => `${SECTION_PREFIX}${sectionId}`;
/** dnd-kit draggable id for a tab (the group/ungroup source). */
export const tabDragId = (panelId: string): string => `${TAB_PREFIX}${panelId}`;
/** dnd-kit droppable id for the gap band before section `index` (`index === len`
 *  is the band below the last section). */
export const gapDropId = (index: number): string => `${GAP_PREFIX}${index}`;

function parsePrefixed(id: DndId, prefix: string): string | null {
  if (typeof id !== "string" || !id.startsWith(prefix)) return null;
  const body = id.slice(prefix.length);
  return body.length > 0 ? body : null;
}

/** Section id from a `section:{id}` drag id, or null. */
export const parseSectionId = (id: DndId): string | null => parsePrefixed(id, SECTION_PREFIX);
/** Panel id from a `tab:{panelId}` drag id, or null. */
export const parseTabId = (id: DndId): string | null => parsePrefixed(id, TAB_PREFIX);
/** Non-negative integer gap index from a `gap:{index}` drop id, or null. */
export function parseGapIndex(id: DndId): number | null {
  const body = parsePrefixed(id, GAP_PREFIX);
  if (body == null) return null;
  const n = Number(body);
  return Number.isInteger(n) && n >= 0 ? n : null;
}

/** A resolved drag intent, or `null` (no-op) from the resolver. */
export type RightPaneDragOp =
  | { kind: "reorder"; sectionId: string; toIndex: number }
  | { kind: "reorderTab"; panelId: string; toIndex: number }
  | { kind: "group"; panelId: string; targetSectionId: string }
  | { kind: "ungroup"; panelId: string };

/**
 * Resolve a dnd-kit drag-end into the model op it triggers, or `null` for a
 * no-op drop (no target, dropped on its own group, an already-solo ungroup, or
 * an unrecognized id). Pure — the gesture's only logic; the caller routes a
 * non-null result through `applyRightPaneDrag`.
 */
export function resolveRightPaneDrag(
  layout: RightPaneLayout,
  activeId: DndId,
  overId: DndId,
): RightPaneDragOp | null {
  if (overId == null) return null;

  // A section is being dragged → reorder.
  const draggedSection = parseSectionId(activeId);
  if (draggedSection != null) {
    const from = layout.sections.findIndex((s) => s.id === draggedSection);
    if (from === -1) return null;

    const overSection = parseSectionId(overId);
    if (overSection != null) {
      const toIndex = layout.sections.findIndex((s) => s.id === overSection);
      if (toIndex === -1 || toIndex === from) return null;
      return { kind: "reorder", sectionId: draggedSection, toIndex };
    }
    const gap = parseGapIndex(overId);
    if (gap != null) {
      // gap N sits between original sections N-1 and N; translate to the
      // post-removal index `moveSection` inserts at.
      const toIndex = gap > from ? gap - 1 : gap;
      if (toIndex === from) return null;
      return { kind: "reorder", sectionId: draggedSection, toIndex };
    }
    return null;
  }

  // A tab is being dragged → reorder within its group (onto a sibling tab),
  // group (onto another section / another section's tab), or ungroup (into a gap).
  const draggedPanel = parseTabId(activeId);
  if (draggedPanel != null) {
    const loc = findPanelLocation(layout, draggedPanel);
    if (!loc) return null;
    const home = layout.sections[loc.sectionIndex];

    // Dropped onto another panel's tab.
    const overTab = parseTabId(overId);
    if (overTab != null) {
      if (overTab === draggedPanel) return null; // self
      const overLoc = findPanelLocation(layout, overTab);
      if (!overLoc) return null;
      if (overLoc.sectionIndex === loc.sectionIndex) {
        // same group → reorder the tab to the target tab's slot
        return { kind: "reorderTab", panelId: draggedPanel, toIndex: overLoc.panelIndex };
      }
      // a tab in another section → group into that section
      return {
        kind: "group",
        panelId: draggedPanel,
        targetSectionId: layout.sections[overLoc.sectionIndex].id,
      };
    }

    const overSection = parseSectionId(overId);
    if (overSection != null) {
      if (overSection === home.id) return null; // dropped on its own group
      return { kind: "group", panelId: draggedPanel, targetSectionId: overSection };
    }
    if (parseGapIndex(overId) != null) {
      // Ungroup only makes sense out of a multi-panel group (AC-3: a solo panel
      // has nothing to split). `splitPanelToOwnSection` places it below the
      // source, so the exact gap slot is not load-bearing.
      if (home.group.panelIds.length <= 1) return null;
      return { kind: "ungroup", panelId: draggedPanel };
    }
  }

  return null;
}

/** Apply a resolved drag op to the layout via the SW-RP-1 model ops. */
export function applyRightPaneDrag(layout: RightPaneLayout, op: RightPaneDragOp): RightPaneLayout {
  switch (op.kind) {
    case "reorder":
      return moveSection(layout, op.sectionId, op.toIndex);
    case "reorderTab":
      return movePanelWithinSection(layout, op.panelId, op.toIndex);
    case "group":
      // Expand the target so the merged-in (now active) tab is actually visible —
      // dropping onto a collapsed section would otherwise hide it behind a title bar.
      return expandSection(
        mergePanelIntoSection(layout, op.panelId, op.targetSectionId),
        op.targetSectionId,
      );
    case "ungroup":
      return splitPanelToOwnSection(layout, op.panelId);
  }
}
