// SPDX-License-Identifier: MIT
import { CircleArrowUp, X } from "lucide-react";

import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { CHROME_ICON, CHROME_ICON_BUTTON } from "@/shared/components/chrome-control";
import { cn } from "@/lib/utils";
import type { MachineSummary } from "@/tauri/commands";

import { openMachinesPanel } from "../lib/machine-actions";
import { outOfDateLabel, useOutOfDateNotice } from "../lib/out-of-date-servers";
import { TONE_CLASS } from "../lib/machine-status";
import { useTeamMachines } from "../stores/machines-store";

/**
 * SW-AGR-19 PR-2 — "N terminal servers out of date", in the Machines header. One
 * calm line (never a toast, never a live region) that stays until the user
 * dismisses it; dismissed, it stays away until another server, or another build,
 * goes out of date. Each stale row below carries its own Update action.
 */
export function TerminalServersNotice({ machines }: { machines: MachineSummary[] }) {
  const { servers, dismiss } = useOutOfDateNotice(machines);
  if (servers.length === 0) return null;
  const label = outOfDateLabel(servers.length);
  return (
    <div
      data-testid="terminal-servers-out-of-date"
      className={cn("flex min-w-0 items-center gap-1.5 text-[11px]", TONE_CLASS.warn)}
    >
      <CircleArrowUp size={12} strokeWidth={1.75} aria-hidden="true" className="shrink-0" />
      <span className="truncate">{label}</span>
      <button
        type="button"
        data-testid="terminal-servers-out-of-date-dismiss"
        aria-label={`Dismiss: ${label}`}
        title="Hide until another server goes out of date"
        onClick={dismiss}
        className="-my-1.5 inline-flex size-6 shrink-0 items-center justify-center rounded text-muted-foreground outline-none transition-colors hover:bg-foreground/[0.07] hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
      >
        <X size={12} strokeWidth={1.75} aria-hidden="true" />
      </button>
    </div>
  );
}

/**
 * The same notice in the title bar's right cluster, always in view: a chrome
 * button that opens Machines, where each stale server offers its update and the
 * notice is dismissed (which hides both — it is one indicator). With room it
 * names itself; narrow, it folds to the icon and count, the words in its label.
 */
export function TerminalServersTitleBarNotice({ teamKey }: { teamKey: string }) {
  const machines = useTeamMachines(teamKey);
  const { servers } = useOutOfDateNotice(machines);
  if (servers.length === 0) return null;
  const label = outOfDateLabel(servers.length);
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          data-testid="titlebar-terminal-servers-out-of-date"
          aria-label={`${label} — open Machines`}
          onClick={() => openMachinesPanel(null)}
          className={cn(
            CHROME_ICON_BUTTON,
            "w-auto min-w-7 gap-1 px-1.5 @[64rem]/titlebar:pr-2",
            TONE_CLASS.warn,
          )}
        >
          <CircleArrowUp {...CHROME_ICON} aria-hidden="true" />
          <span
            aria-hidden="true"
            className="text-xs font-medium tabular-nums @[64rem]/titlebar:hidden"
          >
            {servers.length}
          </span>
          <span aria-hidden="true" className="hidden text-xs font-medium @[64rem]/titlebar:inline">
            {label}
          </span>
        </button>
      </TooltipTrigger>
      <TooltipContent side="bottom">{label}</TooltipContent>
    </Tooltip>
  );
}
