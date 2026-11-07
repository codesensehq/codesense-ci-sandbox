// SPDX-License-Identifier: MIT
import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

/** A small count or state pill. The tone is one of the app's status tokens, or neutral. */
const badgeVariants = cva(
  "inline-flex h-[18px] min-w-[18px] shrink-0 items-center justify-center rounded-full px-1.5 text-[11px] leading-none font-semibold tabular-nums",
  {
    variants: {
      tone: {
        neutral: "bg-muted text-foreground",
        needs: "bg-needs-soft text-needs-ink",
        busy: "bg-busy-soft text-busy-ink",
        failed: "bg-failed-soft text-failed-ink",
        warn: "bg-warning-wash text-warning-ink",
        quiet: "border border-border/70 bg-transparent font-medium text-muted-foreground",
      },
    },
    defaultVariants: { tone: "neutral" },
  },
);

export type BadgeTone = NonNullable<VariantProps<typeof badgeVariants>["tone"]>;

function Badge({
  className,
  tone = "neutral",
  ...props
}: React.ComponentProps<"span"> & VariantProps<typeof badgeVariants>) {
  return (
    <span
      data-slot="badge"
      data-tone={tone}
      className={cn(badgeVariants({ tone }), className)}
      {...props}
    />
  );
}

export { Badge, badgeVariants };
