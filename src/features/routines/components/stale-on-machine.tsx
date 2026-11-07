// SPDX-License-Identifier: MIT
import { machineLabel, useRoutineMachines } from "@/features/routines/lib/use-routine-machines";
import { useRoutinesStore } from "@/features/routines/stores/routines-store";
import type { RoutineRow } from "@/tauri/commands";

/** The reason the backend records when the routine or its agent changed after it was
 *  prepared on its machine (SW-XS-25): only then does the machine still run an older copy. */
const DRIFTED = "changed since it was prepared on the machine";

/**
 * SW-XS-25: a machine-armed routine whose prepared content is behind it. Says why, and
 * offers Refresh, which prepares it on the machine from what it is now. Shown on the
 * agent's Routines rows and on the Schedule.
 */
export function StaleOnMachine({ row }: { row: RoutineRow }) {
  const refreshing = useRoutinesStore((s) => s.pending[`refresh:${row.key}`] === true);
  const refreshOnMachine = useRoutinesStore((s) => s.refreshOnMachine);
  const { labels } = useRoutineMachines();
  const stale = row.content_stale;
  if (!stale) return null;
  const machine = machineLabel(stale.machine, labels);
  const drifted = stale.why === DRIFTED;
  return (
    <div
      data-testid="routine-stale"
      className="mt-0.5 flex flex-wrap items-center gap-x-1.5 gap-y-0.5 text-xs leading-4"
    >
      <span className="rounded-full bg-warning-wash px-1.5 font-medium text-warning-ink">
        Out of date on {machine}
      </span>
      <span data-testid="routine-stale-why" className="text-muted-foreground">
        {drifted
          ? `Changed since it was prepared there; ${machine} runs the earlier version.`
          : `${capitalize(stale.why)}.`}
      </span>
      <button
        type="button"
        disabled={refreshing}
        aria-label={`Refresh ${row.def.name} on ${machine}`}
        onClick={() => void refreshOnMachine(row.key)}
        className="text-foreground underline underline-offset-2 disabled:opacity-50"
      >
        {refreshing ? "Refreshing…" : "Refresh"}
      </button>
    </div>
  );
}

function capitalize(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}
