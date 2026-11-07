// SPDX-License-Identifier: MIT
import * as React from "react";

import { cn } from "@/lib/utils";

/** A thin usage bar: plain until 80% of `max`, warning from there, failed once full. */
function Meter({
  value,
  max,
  label,
  className,
  ...props
}: Omit<React.ComponentProps<"div">, "children"> & { value: number; max: number; label: string }) {
  const ratio = max > 0 ? value / max : 0;
  const level = ratio >= 1 ? "full" : ratio >= 0.8 ? "hot" : "ok";
  const pct = Math.min(100, Math.max(0, ratio * 100));
  return (
    <div
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={max}
      aria-valuenow={value}
      data-slot="meter"
      data-level={level}
      className={cn("h-1.5 w-36 shrink-0 overflow-hidden rounded-full bg-muted", className)}
      {...props}
    >
      <div
        className={cn(
          "h-full rounded-full",
          level === "ok" && "bg-muted-foreground",
          level === "hot" && "bg-warning-fg",
          level === "full" && "bg-[var(--cs-status-failed)]",
        )}
        style={{ width: `${pct}%` }}
      />
    </div>
  );
}

export { Meter };
