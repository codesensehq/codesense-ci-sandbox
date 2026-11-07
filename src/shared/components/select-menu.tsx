// SPDX-License-Identifier: MIT
import { useState } from "react";
import { Check, ChevronDown } from "lucide-react";

import { FloatingMenu, menuOptionClass } from "@/shared/components/floating-menu";

/**
 * SW-UX (app-flow redesign) — the ONE app-styled select dropdown, replacing raw
 * native `<select>` elements (which render the OS picker and look inconsistent with
 * the rest of the chrome). Built on the shared portaled `FloatingMenu` so it matches
 * the nav/connection dropdowns: bordered trigger + chevron, checkmarked options.
 */
export function SelectMenu<T extends string>({
  value,
  options,
  onChange,
  id,
  ariaLabel,
}: {
  value: T;
  options: readonly { value: T; label: string }[];
  onChange: (value: T) => void;
  id?: string;
  ariaLabel?: string;
}) {
  const [open, setOpen] = useState(false);
  const selected = options.find((o) => o.value === value);
  return (
    <FloatingMenu
      open={open}
      onOpenChange={setOpen}
      align="start"
      role="listbox"
      className="w-[var(--radix-popover-trigger-width)] min-w-48"
      trigger={
        <button
          type="button"
          id={id}
          aria-label={ariaLabel}
          aria-haspopup="listbox"
          className="flex h-9 w-full items-center justify-between gap-2 rounded-md border border-border bg-background px-2.5 text-sm hover:bg-accent hover:text-accent-foreground"
        >
          <span className="min-w-0 truncate text-left">{selected?.label ?? "Select…"}</span>
          <ChevronDown size={14} className="shrink-0 text-muted-foreground" aria-hidden="true" />
        </button>
      }
    >
      {options.map((o) => {
        const active = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role="option"
            aria-selected={active}
            className={menuOptionClass(active)}
            style={{ color: "var(--foreground)" }}
            onClick={() => {
              onChange(o.value);
              setOpen(false);
            }}
          >
            <span className="min-w-0 flex-1 truncate text-left">{o.label}</span>
            {active && <Check size={14} className="shrink-0" aria-hidden="true" />}
          </button>
        );
      })}
    </FloatingMenu>
  );
}
