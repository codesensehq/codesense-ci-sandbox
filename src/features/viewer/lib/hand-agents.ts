// SPDX-License-Identifier: MIT
import { isFleetAgent } from "@/features/viewer/lib/agent-rows";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { agentCliLabel } from "@/shared/lib/agent-clis";
import { isRegisteredAgentRuntime } from "@/shared/lib/agent-runtimes";
import { endedByExit, type SessionAgentRecord } from "@/shared/lib/agent-state";
import type { TerminalAiStatus } from "@/shared/lib/terminal-ai-status";
import { markOfAgentStatus, markOfTerminalStatus } from "@/shared/lib/status-mark";
import type { AgentStatus } from "@/shared/lib/agent-status-config";
import { formatMarkLabel } from "@/shared/lib/terminal-dot-words";
import {
  sessionTarget,
  type TerminalLayoutProjection,
} from "@/shared/lib/terminal-layout-projection";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { AgentSessionRow } from "@/tauri/commands";

/**
 * Agents a person started by hand: a supported CLI typed into a plain terminal tab. The
 * launch registry never saw them, so the roster holds at most a bare live-PTY row for their
 * tab. What says an agent runs there is, first, the process tree: the tab-process probe
 * finds a known agent CLI running under the tab's shell, whether or not that agent has hooks
 * and whether or not it was started before this app launched. The session's agent-state
 * record (its hooks and signals) then adds what it is doing. Such a tab is listed, faced and
 * counted like any launched agent.
 */

/** The roster kind a hand-started agent's row carries once it is recognised. */
export const HAND_STARTED_KIND = "hand-started";

/** What the agent-state store says runs in a session. */
export interface DetectedAgent {
  /** The agent's executable (`claude`), when it is known. */
  cli: string | null;
  /** Its live status dot, if it shows one. */
  dot: TerminalAiStatus | null;
  /** Only the process tree says it runs: no hook or signal has spoken for it. */
  processOnly?: boolean;
}

/** A roster row as the agents list and tab strip read it: a hand-started one says what runs. */
export type ListedAgentRow = AgentSessionRow & { detected?: DetectedAgent };

/**
 * Whether an agent runs in a session. The process tree is the ground truth: a known agent
 * CLI the tab-process probe sees running under the session's shell (`processCli`) makes it
 * one, hooks or not, so a session reattached after this app started and a CLI with no hooks
 * (Codex) both count. Its agent-state record speaks too: one established itself (a Claude
 * hook), or one is mid-turn through its MCP signals. A session whose agent ended (its
 * `SessionEnd`, its process exit) and whose shell runs no agent is a plain shell again.
 */
export function detectedAgentCli(
  rec: SessionAgentRecord | undefined,
  launchedAgent: string | undefined,
  processCli?: string | null,
): { cli: string | null; processOnly: boolean } | null {
  if (rec?.agent != null && rec.state !== "done" && !endedByExit(rec))
    return { cli: rec.agent, processOnly: false };
  if (rec && rec.state !== "idle" && rec.state !== "done" && !endedByExit(rec)) {
    return { cli: launchedAgent ?? processCli ?? null, processOnly: false };
  }
  if (processCli) return { cli: processCli, processOnly: true };
  return null;
}

/** Live detection shared by roster projection, controls and keyboard actions. */
export function detectSessionAgent(
  state: Pick<ReturnType<typeof useAgentStateStore.getState>, "bySession"> &
    Partial<
      Pick<
        ReturnType<typeof useAgentStateStore.getState>,
        "launchBySession" | "processAgentBySession" | "activityBySession" | "exitBySession"
      >
    >,
  sessionId: string,
  layout = useViewerStore.getState().terminalLayout,
): ReturnType<typeof detectedAgentCli> {
  if (state.exitBySession?.[sessionId]) return null;
  const activity = state.activityBySession?.[sessionId];
  if (activity && activity.freshness !== "current") {
    // Missing instrumentation on an admitted host is different from lost liveness.
    const process = state.processAgentBySession?.[sessionId];
    if (activity.freshness === "recovering" || !process || !sessionTarget(layout, sessionId))
      return null;
    return { cli: process, processOnly: true };
  }
  const snapshot = activity?.snapshot;
  if (snapshot?.observed && !snapshot.exit) return { cli: snapshot.provider, processOnly: false };
  return detectedAgentCli(
    state.bySession[sessionId],
    state.launchBySession?.[sessionId]?.agent,
    state.processAgentBySession?.[sessionId],
  );
}

/** Only a CodeSense agent CLI exposes a native inbox path. */
export function isRunningAgent(
  state: Parameters<typeof detectSessionAgent>[0],
  sessionId: string,
  layout?: TerminalLayoutProjection,
): boolean {
  const cli = detectSessionAgent(state, sessionId, layout)?.cli;
  return isRegisteredAgentRuntime(cli);
}

export function useRunningAgent(sessionId: string): boolean {
  const layout = useViewerStore((s) => s.terminalLayout);
  return useAgentStateStore((s) => isRunningAgent(s, sessionId, layout));
}

/** A session's agent identity, retained for history and offline presentation.
 * Live inbox controls use isRunningAgent instead. */
export function isAgentSession(
  row: Pick<AgentSessionRow, "alive" | "kind" | "agentRevision" | "detected">,
): boolean {
  return row.alive && isFleetAgent(row);
}

/** The agent's name for people: `claude` → `Claude`. */
export function cliLabel(cli: string | null): string {
  return agentCliLabel(cli);
}

/** What a hand-started agent's row says under its name: its CLI and what it is doing. One
 *  only the process tree knows of cannot say what it is doing: the probe sees the process,
 *  not its turn. */
export function handAgentSubtitle(
  detected: DetectedAgent,
  status?: AgentStatus,
  subagents?: number,
): string {
  const cli = cliLabel(detected.cli);
  if (status)
    return `${cli} · ${formatMarkLabel({ mark: markOfAgentStatus(status), words: status === "dropped" ? "Needs you" : undefined, subagents })}`;
  if (detected.processOnly && (detected.dot === null || detected.dot === "running")) {
    return `${cli} · Activity unknown`;
  }
  if (!detected.dot) return `${cli} · started in this tab`;
  const mark = markOfTerminalStatus(detected.dot);
  return `${cli} · ${formatMarkLabel({ mark, status: detected.dot, subagents })}`;
}
