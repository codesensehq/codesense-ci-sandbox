// SPDX-License-Identifier: MIT
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { AgentRow } from "@/tauri/commands";

import type { AgentRowState, AgentRowStatus } from "../lib/agent-row-state";

const WORD: Record<AgentRowState, string> = {
  needs: "Needs you",
  running: "running",
  broken: "Can't launch",
  archived: "Archived",
  ready: "Ready",
};

/** What a row's state word says when it has live instances: the count, in the state's own words. */
function liveLabel(state: AgentRowState, live: number): string {
  if (state === "running") return `${live} running`;
  return `${WORD[state]} · ${live}`;
}

/**
 * A row's state in a word. One rule, whatever the state: a row with live instances (`live > 0`)
 * shows the word as a button that reveals the live agents list filtered to the agent, so the
 * shortcut never depends on the state; a row with none shows the plain word.
 */
export function StateWord({ row, status }: { row: AgentRow; status: AgentRowStatus }) {
  if (status.live > 0) {
    return (
      <button
        type="button"
        data-testid="agent-running"
        title="Show its live agents"
        className="rounded-sm hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
        onClick={() =>
          useViewerStore.getState().revealAgentsListFor({ agentKey: row.key, name: row.name })
        }
      >
        {liveLabel(status.state, status.live)}
      </button>
    );
  }
  return (
    <span data-testid="agent-state">
      {WORD[status.state] === "running" ? "Ready" : WORD[status.state]}
    </span>
  );
}
