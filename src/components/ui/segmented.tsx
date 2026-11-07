// SPDX-License-Identifier: MIT
import * as React from "react";

import { cn } from "@/lib/utils";

export interface SegmentedOption<V extends string> {
  value: V;
  label: React.ReactNode;
  disabled?: boolean;
  testId?: string;
}

interface SegmentedProps<V extends string> extends Omit<
  React.ComponentProps<"div">,
  "onChange" | "children"
> {
  value: V;
  onValueChange: (value: V) => void;
  options: readonly SegmentedOption<V>[];
  /** Options share the width equally. */
  full?: boolean;
}

/** A short single-choice control (a radiogroup): the selected option is raised on a muted
 *  track. Arrow keys move the selection and the focus. */
function Segmented<V extends string>({
  value,
  onValueChange,
  options,
  full,
  className,
  ...props
}: SegmentedProps<V>) {
  const refs = React.useRef<(HTMLButtonElement | null)[]>([]);
  const onKeyDown = (e: React.KeyboardEvent, from: number) => {
    const step = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : -1;
    if (!["ArrowRight", "ArrowDown", "ArrowLeft", "ArrowUp"].includes(e.key)) return;
    e.preventDefault();
    for (let n = 1; n <= options.length; n += 1) {
      const i = (from + step * n + options.length * n) % options.length;
      if (options[i].disabled) continue;
      onValueChange(options[i].value);
      refs.current[i]?.focus();
      return;
    }
  };
  return (
    <div
      role="radiogroup"
      data-slot="segmented"
      className={cn("gap-0.5 rounded-lg bg-muted p-0.5", full ? "flex" : "inline-flex", className)}
      {...props}
    >
      {options.map((o, i) => {
        const on = o.value === value;
        return (
          <button
            key={o.value}
            ref={(el) => {
              refs.current[i] = el;
            }}
            type="button"
            role="radio"
            aria-checked={on}
            disabled={o.disabled}
            tabIndex={on ? 0 : -1}
            data-testid={o.testId}
            onClick={() => onValueChange(o.value)}
            onKeyDown={(e) => onKeyDown(e, i)}
            className={cn(
              "inline-flex h-7 items-center justify-center gap-1.5 rounded-md px-3 text-[13px] font-medium whitespace-nowrap text-muted-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50",
              full && "flex-1",
              on && "bg-background text-foreground shadow-sm",
            )}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

export { Segmented };
