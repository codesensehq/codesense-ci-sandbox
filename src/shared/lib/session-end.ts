// SPDX-License-Identifier: MIT
import { formatEndedLabel } from "./terminal-dot-words";

export type SessionEnd = "ended" | "crashed";

/** An exit as the agent-state store's exit event or a tab's layout entry holds it. */
interface ExitFacts {
  code: number | null;
  signal?: string | null;
}

/** How a session's process ended, for its marks: `end` and the words for it. */
export interface SessionEndInfo {
  end: SessionEnd;
  /** `Crashed · SIGKILL`, `Ended · exit 0` — the ended bar's words. */
  label: string;
  /** The same words for an accessible name: `Crashed, SIGKILL`. */
  words: string;
}

/**
 * How a tab's process ended, or null while it runs or when CodeSense ended it.
 * The exit event (`exit`, with its `outcome`) is the fuller source; when only
 * the tab's layout entry has it (after a reload), a signal death is the crash.
 * An agent crash that still stands (`agentCrashed`: the agent died in the
 * shell before the shell ended) marks the tab crashed either way.
 */
export function sessionEndInfo(
  exit: (ExitFacts & { requested?: boolean; outcome?: string }) | undefined,
  layoutExit: ExitFacts | undefined,
  agentCrashed = false,
): SessionEndInfo | null {
  if (exit) {
    if (exit.requested) return null;
    return endInfo(exit.outcome === "crashed" || agentCrashed ? "crashed" : "ended", exit);
  }
  if (!layoutExit) return null;
  return endInfo(layoutExit.signal || agentCrashed ? "crashed" : "ended", layoutExit);
}

function endInfo(end: SessionEnd, exit: ExitFacts): SessionEndInfo {
  return { end, label: formatEndedLabel(end, exit), words: formatEndedLabel(end, exit, ", ") };
}
