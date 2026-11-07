// SPDX-License-Identifier: MIT
import { cn } from "@/lib/utils";
import type { ConnectionPhase } from "./types";

/**
 * Shared connection-state dot (no unicode glyph — project-context: Lucide/CSS
 * only). A small filled circle toned by the three-phase connection. Reused by
 * the ownership chip AND V1's first-class connection indicator (DRY — one tone
 * source). Decorative; the adjacent text label carries the meaning.
 */
const TONE_CLASS: Record<ConnectionPhase, string> = {
  live: "bg-emerald-500",
  reconnecting: "bg-amber-500 motion-safe:animate-pulse",
  disconnected: "bg-muted-foreground/40",
};

export function ConnectionDot({
  connection,
  className,
}: {
  connection: ConnectionPhase;
  className?: string;
}): React.JSX.Element {
  return (
    <span
      aria-hidden
      data-connection={connection}
      className={cn("inline-block size-2 shrink-0 rounded-full", TONE_CLASS[connection], className)}
    />
  );
}
