// SPDX-License-Identifier: MIT
import { sendPromptToAgent, type AgentSessionRow } from "@/tauri/commands";

/** What the person is told after sending a note's text to an agent. */
export type AgentSendOutcome =
  | { kind: "sent" }
  /** It may have reached the agent: never sent again on its own. */
  | { kind: "uncertain"; message: string }
  | { kind: "failed"; message: string };

/**
 * Send `text` to a picked agent and submit it, wherever its tab runs. The target is the
 * roster row the picker showed (its handle and session), resolved in the backend from the
 * same launch registry the roster reads, so an agent on a machine is reached through the
 * machine's agent input, not looked up among this Mac's sessions.
 */
export async function sendToAgent(
  target: { label: string; row: Pick<AgentSessionRow, "handle" | "sessionId"> },
  text: string,
): Promise<AgentSendOutcome> {
  const { label, row } = target;
  let result;
  try {
    result = await sendPromptToAgent(row.handle, row.sessionId, text);
  } catch (e) {
    return { kind: "failed", message: `Couldn't send to ${label}: ${errorText(e)}` };
  }
  switch (result.status) {
    case "submitted":
    case "typed":
      return { kind: "sent" };
    case "unknown":
      return uncertain(label, result.machine);
    default: {
      // Anything that does not say "not submitted" may have reached the agent.
      if (result.submitted !== false) return uncertain(label, result.machine);
      const why =
        result.message?.trim().replace(/\.+$/, "") ||
        (result.machine ? `${result.machine} did not take it` : null);
      return {
        kind: "failed",
        message: `Couldn't send to ${label}${why ? `: ${why}` : ""}. Nothing was sent.`,
      };
    }
  }
}

function uncertain(label: string, machine: string | null): AgentSendOutcome {
  const where = machine ? ` on ${machine}` : "";
  return {
    kind: "uncertain",
    message: `Sent to ${label}${where}, but couldn't confirm it arrived. Check its terminal before sending again.`,
  };
}

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
