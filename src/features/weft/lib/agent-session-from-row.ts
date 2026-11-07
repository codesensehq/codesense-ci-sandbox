// SPDX-License-Identifier: MIT
import type { AgentSessionRow } from "@/tauri/commands";

/**
 * SW-AV-5 — build the minimal `AgentSessionRow`-shaped handle the shipped `useAgentDrill`
 * consumes from an opaque live session id. Reused to jump to (or take the wheel of) an agent
 * behind an Inbox row. Lives in a lib (not the panel) so the action hook and the panel can
 * both import it without a module cycle.
 */
export function agentSessionFromRow(liveSessionId: string): AgentSessionRow {
  return {
    handle: liveSessionId,
    sessionId: liveSessionId,
    kind: "agent",
    alive: true,
    backgrounded: false,
    startedWith: null,
    startedAtMs: null,
    launcher: null,
    lastOutputMs: null,
  };
}
