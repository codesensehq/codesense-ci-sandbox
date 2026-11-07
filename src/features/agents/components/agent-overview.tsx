// SPDX-License-Identifier: MIT
import type { AgentDetailView, AgentRow } from "@/tauri/commands";

import { AgentFacts } from "./agent-facts";

/**
 * The selected agent at a glance: the fact sheet. Each fact opens the Edit section that changes
 * it; what is wrong with one is a line on that fact, never a banner. Display only; nothing here
 * is saved. The routines and the spend cap follow it as plain sections of the Overview.
 */
export function AgentOverview({ row, detail }: { row: AgentRow; detail: AgentDetailView }) {
  return (
    <section aria-label="Overview" data-testid="agent-overview" className="flex flex-col gap-3">
      <AgentFacts row={row} detail={detail} />
    </section>
  );
}
