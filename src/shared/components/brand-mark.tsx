// SPDX-License-Identifier: MIT
import { cn } from "@/lib/utils";

/** The product mark above a first-use headline (splash, sign-in, join). */
export function BrandMark({ className }: { className?: string }) {
  return (
    <div
      data-slot="brand-mark"
      aria-hidden="true"
      className={cn(
        "mb-3 inline-flex size-10 items-center justify-center rounded-lg bg-foreground/10",
        className,
      )}
    >
      <span className="text-lg font-bold">C</span>
    </div>
  );
}
