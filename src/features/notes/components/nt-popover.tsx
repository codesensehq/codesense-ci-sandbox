// SPDX-License-Identifier: MIT
import { Popover as PopoverPrimitive } from "radix-ui";
import type { ComponentProps, KeyboardEvent } from "react";

import { cn } from "@/lib/utils";

/**
 * The notes pane's floats. Rendered in place (no portal), so they inherit the pane's
 * viewer-theme ground and sit on the same surface as the sheet; Radix still owns
 * placement, focus and dismissal.
 */
export const NtPopover = PopoverPrimitive.Root;
export const NtPopoverTrigger = PopoverPrimitive.Trigger;
export const NtPopoverAnchor = PopoverPrimitive.Anchor;

export function NtPopoverContent({
  className,
  sideOffset = 6,
  collisionPadding = 8,
  align = "end",
  onKeyDown,
  ...props
}: ComponentProps<typeof PopoverPrimitive.Content>): React.JSX.Element {
  return (
    <PopoverPrimitive.Content
      sideOffset={sideOffset}
      collisionPadding={collisionPadding}
      align={align}
      className={cn("nt-pop", className)}
      {...props}
      onKeyDown={(e) => {
        menuArrowKeys(e);
        onKeyDown?.(e);
      }}
    />
  );
}

/** Up/down move focus between a float's rows (buttons marked `data-nt-row`). */
export function menuArrowKeys(e: KeyboardEvent<HTMLElement>): void {
  if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
  const target = e.target as HTMLElement;
  if (target.closest("textarea, select, input")) return;
  const rows = Array.from(
    e.currentTarget.querySelectorAll<HTMLElement>("[data-nt-row]:not(:disabled)"),
  );
  if (rows.length === 0) return;
  e.preventDefault();
  const at = rows.indexOf(document.activeElement as HTMLElement);
  const next =
    e.key === "ArrowDown"
      ? rows[(at + 1) % rows.length]
      : rows[(at - 1 + rows.length) % rows.length];
  next.focus();
}
