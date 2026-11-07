// SPDX-License-Identifier: MIT
import * as React from "react";

import { cn } from "@/lib/utils";

/** A quiet placeholder block for content that is loading. Size it with `className`. */
function Skeleton({
  className,
  round,
  ...props
}: React.ComponentProps<"div"> & { round?: boolean }) {
  return (
    <div
      aria-hidden="true"
      data-slot="skeleton"
      className={cn(
        "animate-pulse bg-[color-mix(in_srgb,var(--foreground)_7%,var(--background))] motion-reduce:animate-none",
        round ? "rounded-full" : "rounded-md",
        className,
      )}
      {...props}
    />
  );
}

export { Skeleton };
