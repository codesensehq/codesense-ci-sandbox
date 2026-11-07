// SPDX-License-Identifier: MIT
/**
 * The run whose output the bottom agent area is showing.
 *
 * The exported `openAgentResult(…)` opener sets the target here and reveals the terminal column;
 * the run-output host in that column reads it. `run_id` is the durable key (always present);
 * `sessionId` is optional live enrichment only while the run is still in-flight. `teamKey` pins the
 * view to the team that was active when it opened, so a backgrounded team's workspace never shows it.
 *
 * `storyId` is required alongside `runId` to resolve the Evidence view via `read_run_evidence`
 * (which refuses evidence by `run_id` alone), and `defaultView` picks the tab the view opens on.
 */
import { create } from "zustand";

import type { LayoutMode } from "@/shared/stores/viewer-store";

/** The CLI's layout mode and visibility (one setting for every team) as they were before the
 *  output revealed the column. A `null` mode had never been settled. */
export interface TerminalLayoutSnapshot {
  mode: LayoutMode | null;
  visible: boolean;
}

/** The view a run detail opens on. */
export type RunDetailView = "output" | "findings" | "evidence" | "endorsement";

interface AgentResultStoreState {
  /** The durable run key being shown — `null` when no run output is open. */
  runId: string | null;
  /** Optional live-session key — present only for an in-flight run; `null` when completed. */
  sessionId: string | null;
  /** The run's `story_id`, needed to resolve Evidence; `null` when unresolvable. */
  storyId: string | null;
  /** Which tab the view opens on — `output` unless an opener requests another directly. */
  defaultView: RunDetailView;
  /** The team whose agent area shows the run; `null` when none is open. */
  teamKey: string | null;
  /** The terminal column as it was before opening the run had to reveal it, so closing can put it
   *  back; `null` when the column was already visible or the user has since changed it. */
  restoreTerminal: TerminalLayoutSnapshot | null;
  /** Reveal the output while leaving keyboard focus on the calling surface. */
  preserveFocus: boolean;
  setTarget: (
    runId: string | null,
    sessionId?: string | null,
    storyId?: string | null,
    defaultView?: RunDetailView,
    teamKey?: string | null,
    restoreTerminal?: TerminalLayoutSnapshot | null,
    preserveFocus?: boolean,
  ) => void;
  /** Drop the pending restore: the user took charge of the terminal column. */
  forfeitRestore: () => void;
  /** Dismiss the run output, handing the agent area back to the CLI tiles. */
  close: () => void;
}

export const useAgentResultStore = create<AgentResultStoreState>((set) => ({
  runId: null,
  sessionId: null,
  storyId: null,
  defaultView: "output",
  teamKey: null,
  restoreTerminal: null,
  preserveFocus: false,
  setTarget: (
    runId,
    sessionId = null,
    storyId = null,
    defaultView = "output",
    teamKey = null,
    restoreTerminal = null,
    preserveFocus = false,
  ) => set({ runId, sessionId, storyId, defaultView, teamKey, restoreTerminal, preserveFocus }),
  forfeitRestore: () => set({ restoreTerminal: null }),
  close: () =>
    set({
      runId: null,
      sessionId: null,
      storyId: null,
      teamKey: null,
      restoreTerminal: null,
      preserveFocus: false,
    }),
}));
