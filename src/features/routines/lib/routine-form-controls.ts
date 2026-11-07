// SPDX-License-Identifier: MIT
import { cn } from "@/lib/utils";

export const FIELD = "bg-background text-foreground";
export const SELECT = cn(
  "h-8 rounded-lg border border-input px-2 text-sm outline-none focus-visible:border-ring disabled:opacity-60",
  FIELD,
);
export const PREVIEW_DEBOUNCE_MS = 250;
