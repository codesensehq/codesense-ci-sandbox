// SPDX-License-Identifier: MIT
import { X } from "lucide-react";

import { useRoutinesStore } from "../stores/routines-store";

/**
 * The honest always-on strip at the top of the Agents tab's Schedule (SW-AGR-5, design
 * §4.9). Routines on This Mac fire only while the app runs, so it says so every time, and it counts the occurrences
 * that were skipped while the app was closed or asleep until they are
 * dismissed. Opening the tab never clears that count.
 */
export function RoutinesStatusStrip() {
  const missed = useRoutinesStore((s) => s.missedCount);
  const acknowledging = useRoutinesStore((s) => s.pending.ack === true);
  const acknowledge = useRoutinesStore((s) => s.acknowledgeMissed);
  return (
    <div
      data-testid="routines-status-strip"
      role="status"
      className="flex items-center gap-2 border-b border-border bg-muted/40 px-4 py-2 text-xs text-muted-foreground"
    >
      <span className="min-w-0 flex-1">
        <span className="text-foreground">Routines on This Mac run while CodeSense is open.</span>{" "}
        Wakes and timers for sessions on other machines run there, even when this Mac is closed.
        {missed > 0 && (
          <span data-testid="routines-missed" className="text-foreground">
            {" "}
            · {missed} missed while closed or asleep (skipped)
          </span>
        )}
      </span>
      {missed > 0 && (
        <button
          type="button"
          data-testid="routines-missed-dismiss"
          aria-label="Dismiss missed runs"
          disabled={acknowledging}
          onClick={() => void acknowledge()}
          className="inline-flex h-6 w-6 items-center justify-center rounded-md hover:bg-accent hover:text-foreground disabled:opacity-50"
        >
          <X size={14} aria-hidden="true" />
        </button>
      )}
    </div>
  );
}
