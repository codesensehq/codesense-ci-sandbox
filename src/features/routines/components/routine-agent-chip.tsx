// SPDX-License-Identifier: MIT
import type { RoutineRow } from "@/tauri/commands";

/** The warning a routine row carries when no agent on this Mac answers to its agent
 *  reference (deleted, or in a team this Mac does not have): its runs fail until it
 *  names another. */
export function UnresolvableAgentChip({ row }: { row: Pick<RoutineRow, "agent_display" | "def"> }) {
  if (row.def.action === "command" || row.agent_display.resolvable) return null;
  return (
    <span
      data-testid="routine-agent-unavailable"
      title="No agent on this Mac answers to this: it was deleted, or it lives in a team this Mac doesn't have. Its runs fail until the routine names another agent."
      className="shrink-0 rounded-full bg-warning-wash px-1.5 text-[11px] font-semibold text-warning-ink"
    >
      Agent unavailable: {row.def.agent}
    </span>
  );
}
