// SPDX-License-Identifier: MIT
import type { CSSProperties, ReactNode } from "react";
import { Check } from "lucide-react";

import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";

/**
 * SW-FX-79 — the ONE portaled, collision-aware menu shell. Every hand-rolled
 * `absolute top-full z-*` picker converges here so positioning + dismissal +
 * portal live in a single place (Radix `Popover`: `Portal` + `avoidCollisions`).
 *
 * Non-scope: this does NOT restyle — the shared menu look below is byte-for-byte
 * the look the retired pickers already rendered (`py-1 rounded-md shadow-lg
 * border` + `var(--popover)` bg / `var(--border)` border).
 */

/** Shared dropdown menu background/border — the same CSS-var contract every
 *  retired picker used. Kept as a constant so no picker re-declares it. */
export const MENU_STYLE = {
  backgroundColor: "var(--popover, var(--card))",
  borderColor: "var(--border)",
} as const;

/** Shared option-row styling keeps focus separate from the committed value. */
export const MENU_OPTION_CLASS =
  "flex w-full items-center gap-2 px-2.5 py-1 text-xs text-foreground hover:bg-[color-mix(in_srgb,var(--foreground)_9%,transparent)] focus-visible:outline focus-visible:outline-[1.5px] focus-visible:-outline-offset-[1.5px] focus-visible:outline-ring";

export function menuOptionClass(active: boolean): string {
  return cn(MENU_OPTION_CLASS, active && "font-medium");
}

/** Reserve the check's width even when this option is not selected. */
export function MenuCheck({ checked }: { checked: boolean }) {
  return (
    <span data-menu-check className="ml-auto inline-flex size-3 shrink-0" aria-hidden="true">
      {checked && <Check size={12} />}
    </span>
  );
}

export interface FloatingMenuProps {
  /** Controlled open state (parent owns the boolean, as the pickers already do). */
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** The trigger control — a focusable element (button); wrapped in `asChild`. */
  trigger: ReactNode;
  /** Menu rows/content. */
  children: ReactNode;
  align?: "start" | "center" | "end";
  side?: "top" | "right" | "bottom" | "left";
  /** Overrides for the shared menu container (width/height/padding/overflow). */
  className?: string;
  style?: CSSProperties;
  /** Callers that own their input focus (assignee filter / status combobox) pass
   *  Radix's `onOpenAutoFocus` suppressor so Radix doesn't steal focus on open. */
  onOpenAutoFocus?: (e: Event) => void;
  /** Semantic role for the menu container (e.g. "listbox"); defaults to Radix's. */
  role?: string;
  ariaLabel?: string;
}

export function FloatingMenu({
  open,
  onOpenChange,
  trigger,
  children,
  align = "start",
  side = "bottom",
  className,
  style,
  onOpenAutoFocus,
  role,
  ariaLabel,
}: FloatingMenuProps): ReactNode {
  return (
    <Popover open={open} onOpenChange={onOpenChange}>
      <PopoverTrigger asChild>{trigger}</PopoverTrigger>
      <PopoverContent
        align={align}
        side={side}
        onOpenAutoFocus={onOpenAutoFocus}
        role={role}
        aria-label={ariaLabel}
        className={cn("w-auto gap-0 rounded-md p-0 py-1 text-xs shadow-lg ring-0", className)}
        style={{ ...MENU_STYLE, ...style }}
      >
        {children}
      </PopoverContent>
    </Popover>
  );
}
