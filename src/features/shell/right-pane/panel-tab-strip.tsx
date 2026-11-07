// SPDX-License-Identifier: MIT
/**
 * SW-RP-1: the compact in-pane tab strip for a multi-panel section.
 * SW-RP-2: each tab is also a dnd-kit draggable (`tab:{panelId}`) — the
 * group/ungroup drag source — while still selecting on click. Activation is
 * distance-gated (6px) by the right-pane `DndContext`, so a tap selects and a
 * drag moves; the button keeps `stopPropagation` so neither bubbles to the
 * section header's collapse toggle.
 *
 * NOT Radix `ui/tabs.tsx` — that unmounts inactive content, which would destroy
 * react-flow / editor state on every switch. This strip only emits selection;
 * the stack keeps all panel bodies mounted.
 */
import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";

import { usePanelDragSource } from "./use-panel-drag-source";

export interface PanelTab {
  id: string;
  title: string;
  icon: LucideIcon;
  /** SW-AM-5 (AC-5): optional presence/count badge rendered after the title (the
   *  Agents session count), so it shows on the tab even in a grouped strip. */
  badge?: () => ReactNode;
}

interface PanelTabStripProps {
  tabs: PanelTab[];
  activeId: string;
  onSelect: (id: string) => void;
}

export function PanelTabStrip({ tabs, activeId, onSelect }: PanelTabStripProps) {
  return (
    <div className="flex min-w-0 items-center gap-1 overflow-hidden whitespace-nowrap text-xs text-muted-foreground">
      {tabs.map((tab, i) => (
        <div key={tab.id} className="flex flex-shrink-0 items-center gap-1">
          {i > 0 && <span className="text-muted-foreground/50">|</span>}
          <DraggableTab tab={tab} active={tab.id === activeId} onSelect={onSelect} />
        </div>
      ))}
    </div>
  );
}

function DraggableTab({
  tab,
  active,
  onSelect,
}: {
  tab: PanelTab;
  active: boolean;
  onSelect: (id: string) => void;
}) {
  const Icon = tab.icon;
  const { setNodeRef, dragHandleProps, isDragging, isOver } = usePanelDragSource(tab.id);
  return (
    <button
      type="button"
      ref={setNodeRef}
      {...dragHandleProps}
      onClick={(e) => {
        // Don't let the click bubble to the section header's collapse/expand toggle.
        e.stopPropagation();
        onSelect(tab.id);
      }}
      style={
        isOver && !isDragging
          ? { boxShadow: "inset 0 0 0 1px color-mix(in srgb, var(--primary) 60%, transparent)" }
          : undefined
      }
      className={`flex items-center gap-1 px-1 py-0.5 rounded transition-colors touch-none ${
        active ? "text-foreground font-medium" : "hover:text-foreground cursor-grab"
      } ${isDragging ? "opacity-50" : ""}`}
      aria-pressed={active}
    >
      <Icon size={12} />
      <span>{tab.title}</span>
      {tab.badge?.()}
    </button>
  );
}
