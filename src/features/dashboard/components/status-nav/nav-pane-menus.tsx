// SPDX-License-Identifier: MIT
import type { ComponentProps, CSSProperties } from "react";

import { DropdownMenu, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { StatusSelectorPopover } from "@/shared/components/status-selector-popover";
import type { FrameAnchorRect } from "@/features/shell/host-menu-context";
import { NavFilterMenu } from "./nav-filter-field";

const anchorStyle = (rect: FrameAnchorRect): CSSProperties => ({
  position: "fixed",
  left: rect.x,
  top: rect.y,
  width: rect.width,
  height: rect.height,
  pointerEvents: "none",
});

/**
 * The nav's filter menu, opened at a button that lives outside the native body (the Files
 * dashboard's own). The nav owner renders it, so it exists whenever that owner does.
 */
export function PaneNavFilterMenu({
  anchor,
  onClose,
  ...menu
}: { anchor: FrameAnchorRect; onClose: () => void } & ComponentProps<typeof NavFilterMenu>) {
  return (
    <DropdownMenu open onOpenChange={(open) => !open && onClose()}>
      <DropdownMenuTrigger asChild>
        <span aria-hidden style={anchorStyle(anchor)} />
      </DropdownMenuTrigger>
      <NavFilterMenu {...menu} />
    </DropdownMenu>
  );
}

/** A story's status picker, opened at a mark that lives outside the native body. */
export function PaneNavStatusPicker({
  anchor,
  onClose,
  ...picker
}: { anchor: FrameAnchorRect; onClose: () => void } & Omit<
  ComponentProps<typeof StatusSelectorPopover>,
  "open" | "onOpenChange" | "children"
>) {
  return (
    <span aria-hidden style={anchorStyle(anchor)}>
      <StatusSelectorPopover {...picker} open onOpenChange={(open) => !open && onClose()}>
        <span className="block" style={{ width: anchor.width, height: anchor.height }} />
      </StatusSelectorPopover>
    </span>
  );
}
