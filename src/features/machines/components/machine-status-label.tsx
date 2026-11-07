// SPDX-License-Identifier: MIT
import { Laptop, Server } from "lucide-react";

import { cn } from "@/lib/utils";
import type { MachineSummary } from "@/tauri/commands";

import { TONE_CLASS, type MachineStatusView } from "../lib/machine-status";

/** Glyph + word (+ detail): readable without color — the tone only tints it. */
export function MachineStatusLabel({
  view,
  className,
  testId,
}: {
  view: MachineStatusView;
  className?: string;
  testId?: string;
}) {
  return (
    <span
      data-testid={testId}
      className={cn(
        "inline-flex min-w-0 items-center gap-1 text-[11px]",
        TONE_CLASS[view.tone],
        className,
      )}
    >
      <span aria-hidden="true">{view.glyph}</span>
      <span className="truncate">
        {view.word}
        {view.detail ? ` · ${view.detail}` : ""}
      </span>
    </span>
  );
}

/** `⚑ N needs you` — agents on a machine waiting on the user. Compact form (`⚑N`)
 *  rides the collapsed picker segment. Renders nothing at zero. */
export function AttentionBadge({ count, compact = false }: { count: number; compact?: boolean }) {
  if (count <= 0) return null;
  const label = `${count} needs you`;
  return (
    <span
      data-testid="machine-attention"
      aria-label={label}
      title={label}
      className="inline-flex shrink-0 items-center gap-0.5 rounded bg-amber-500/15 px-1 text-[11px] font-medium text-amber-700 dark:text-amber-300"
    >
      <span aria-hidden="true">⚑</span>
      {compact ? count : ` ${label}`}
    </span>
  );
}

/** The machine-kind glyph: a laptop for This Mac, a server for an SSH machine. */
export function MachineIcon({ machine, size = 14 }: { machine: MachineSummary; size?: number }) {
  const Icon = machine.kind === "local" ? Laptop : Server;
  return <Icon size={size} className="shrink-0" aria-hidden="true" />;
}
