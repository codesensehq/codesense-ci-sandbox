// SPDX-License-Identifier: MIT
import { useCallback } from "react";

import type { PipelineStatus } from "@/shared/lib/pipeline-graph/types";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import { StageRailShape } from "@/shared/components/stage-rail";
import { FloatingMenu, menuOptionClass, MenuCheck } from "@/shared/components/floating-menu";

interface StatusSelectorPopoverProps {
  /** Current story status — highlighted in the menu. */
  currentStatus: PipelineStatus;
  /** Callback when a new status is selected. Only fires if it differs from current. */
  onSelect: (status: PipelineStatus) => void;
  /** Controlled open state. */
  open: boolean;
  /** Called when the popover should open or close. */
  onOpenChange: (open: boolean) => void;
  /** Trigger element rendered inside the wrapper. Clicking it toggles the popover. */
  children: React.ReactNode;
  /** When true, the trigger button is disabled and the popover cannot open. */
  disabled?: boolean;
  /** Accessible name for the trigger button. Needed when `children` is an
   *  icon-only glyph (e.g. a bare stage rail), whose `title` attribute is
   *  not exposed as the button's accessible name to assistive tech. */
  triggerAriaLabel?: string;
}

/**
 * Status selector popover used by the pipeline status nav. Renders a trigger button wrapping `children`
 * and a dropdown menu of all pipeline statuses.
 *
 * Fully controlled: the consumer owns `open` / `onOpenChange` so it can
 * coordinate with other popovers (e.g. supporting-docs in the canvas node).
 *
 */
export function StatusSelectorPopover({
  currentStatus,
  onSelect,
  open,
  onOpenChange,
  children,
  disabled,
  triggerAriaLabel,
}: StatusSelectorPopoverProps) {
  // SW-GT-3: the switcher options ARE the live taxonomy states (in `order`), not a
  // hardcoded list. Subscribing repaints the menu on team switch / taxonomy edit.
  const states = useStatusTaxonomyStore((s) => s.states);

  const offTaxonomy = !states.some((state) => state.id === currentStatus);

  const handleSelect = useCallback(
    (s: PipelineStatus) => {
      onOpenChange(false);
      if (s !== currentStatus) onSelect(s);
    },
    [currentStatus, onSelect, onOpenChange],
  );

  return (
    <FloatingMenu
      open={open}
      onOpenChange={onOpenChange}
      trigger={
        <button
          type="button"
          aria-label={triggerAriaLabel}
          aria-haspopup="menu"
          // SW-FX-81: hug the child (icon or w-fit pill), never fill the row.
          // A full-width trigger takes the whole flex-basis and starves an
          // adjacent flex-1 title (the nav StoryRow) to zero width. `shrink-0`
          // keeps the icon-sized trigger intact so the title absorbs truncation.
          className="inline-flex shrink-0 text-left"
          disabled={disabled}
          onClick={(e) => e.stopPropagation()}
        >
          {children}
        </button>
      }
      role="menu"
      ariaLabel={offTaxonomy ? `Status, current: ${currentStatus}` : "Status"}
      className="overflow-y-auto"
      style={{ minWidth: 140, maxHeight: 200 }}
    >
      {offTaxonomy && (
        <div
          role="none"
          className="mb-1 flex items-center gap-2 border-b border-border px-2.5 py-1 text-muted-foreground"
        >
          <StageRailShape role={undefined} />
          <span>
            Current: <span className="font-mono">{currentStatus}</span>
          </span>
        </div>
      )}
      {states.map((state) => {
        const isActive = state.id === currentStatus;
        return (
          <button
            key={state.id}
            type="button"
            className={menuOptionClass(isActive)}
            role="menuitemradio"
            aria-checked={isActive}
            onClick={(e) => {
              e.stopPropagation();
              handleSelect(state.id);
            }}
          >
            <StageRailShape role={state.role} />
            <span>{state.label}</span>
            <MenuCheck checked={isActive} />
          </button>
        );
      })}
    </FloatingMenu>
  );
}
