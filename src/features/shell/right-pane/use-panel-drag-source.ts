// SPDX-License-Identifier: MIT
/**
 * SW-RP-2: a panel label (a tab, or a single-panel section title) as a dnd-kit
 * drag source AND drop target under the same `tab:{panelId}` id — so a tab can be
 * dragged (group / ungroup / reorder) and also be dropped onto (reorder a sibling
 * within the group, or group in from another section). Centralizes the id + the
 * attributes/listeners spread + the combined ref so the tab strip and the
 * single-panel title can't drift apart. Kept out of the pure `right-pane-dnd.ts`.
 */
import { useDraggable, useDroppable } from "@dnd-kit/core";

import { tabDragId } from "./right-pane-dnd";

export function usePanelDragSource(panelId: string) {
  const id = tabDragId(panelId);
  const { setNodeRef: setDragRef, listeners, attributes, isDragging } = useDraggable({ id });
  const { setNodeRef: setDropRef, isOver } = useDroppable({ id });
  const setNodeRef = (el: HTMLElement | null) => {
    setDragRef(el);
    setDropRef(el);
  };
  return { setNodeRef, dragHandleProps: { ...attributes, ...listeners }, isDragging, isOver };
}
