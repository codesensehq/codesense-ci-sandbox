// SPDX-License-Identifier: MIT
import type { ButtonHTMLAttributes, ReactNode } from "react";

/**
 * SW-SN-14 — the shared full-width menu-item row used across the notes chrome:
 * the `Popover` menus (rail row ⋯ and panel header ⋯) AND the rail's empty-state
 * "Create note" action row. One class string instead of the six hand-copied
 * `flex w-full … hover:bg-accent …` buttons that had drifted. Icon + label;
 * `disabled` / `onClick` / `aria-*` flow through the rest-spread.
 */
export function PopoverMenuItem({
  icon,
  children,
  ...rest
}: {
  icon: ReactNode;
  children: ReactNode;
} & Omit<ButtonHTMLAttributes<HTMLButtonElement>, "className" | "type">) {
  return (
    <button
      type="button"
      className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-xs hover:bg-accent hover:text-accent-foreground disabled:opacity-40"
      {...rest}
    >
      {icon}
      {children}
    </button>
  );
}
