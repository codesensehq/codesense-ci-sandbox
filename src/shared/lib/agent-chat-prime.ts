// SPDX-License-Identifier: MIT
import { injectAgentLine, type InjectOutcome } from "@/shared/lib/agent-session-injection";
import { logger } from "@/shared/lib/logger";

/**
 * SW-GS-2d (AC-0/AC-3b) — the shared, agent-agnostic chat priming + handshake
 * ping. ALL bind paths — connect, reconnect-reattach, and click-select —
 * re-inject the SAME instruction exactly once per (re)bind (AC-0). A fresh/
 * relaunched agent context loses the "talk back via the UI" instruction;
 * re-priming on every bind makes the comms layer self-healing.
 *
 * The prime IS the ping (AC-3b): it carries a one-shot `token` the agent is asked
 * to echo through `post_agent_message`, so CodeSense can correlate the pong to
 * this bind and gate "connected" on a proven round-trip — not raw process
 * liveness. No agent-specific literal here (AC-4a) — it's reused verbatim by any
 * workspace agent surface.
 */

/** A short, collision-resistant one-shot token for one handshake attempt. The
 *  agent echoes it back in its pong so an uncorrelated/stale message can't
 *  satisfy the handshake. */
export function newHandshakeToken(): string {
  return `cs-ack-${crypto.randomUUID().slice(0, 8)}`;
}

/**
 * The exact line injected into the live agent. Pure, so the token + session-id +
 * post_agent_message instruction are unit-asserted. The git-sync agent has Bash
 * DENIED, so it cannot read its own `$CODESENSE_TAB_SESSION_ID` — CodeSense hands
 * it the LITERAL session id here so it can self-address `post_agent_message`.
 *
 * Submission is made reliable by the handshake's Enter-nudge (agent-handshake),
 * NOT by keeping this short — so the line is EMPHATIC about persistence: use
 * `post_agent_message` for EVERY reply, not just the confirm (the agent otherwise
 * drifts back to terminal output after the first reply, which the user never
 * sees). It also hands over the per-session id + a confirm token. NOTE: this is a
 * one-time message; if drift persists across a long conversation, a persistent
 * `CLAUDE.md` instruction (re-injected each turn) is the durable follow-up.
 */
export function buildPrimeLine(sessionId: string, token: string): string {
  return (
    `[CodeSense] You're connected to the CodeSense UI chat — the user reads ONLY the chat, ` +
    `NOT this terminal. So for EVERY reply to the user, call the post_agent_message MCP tool ` +
    `with sessionId "${sessionId}" (use this exact id every time). Confirm now by posting a ` +
    `brief hello that includes the token ${token}`
  );
}

/** Injected seam so the wrapper unit-tests without a real PTY. */
export interface PrimeDeps {
  inject: (
    sessionId: string,
    line: string,
    deps?: undefined,
    opts?: { gate?: "shell" | "none" | "auto" },
  ) => Promise<InjectOutcome>;
}

const defaultPrimeDeps: PrimeDeps = { inject: injectAgentLine };

/**
 * Inject the prime/ping line into the CONFIRMED-LIVE agent (gate "none" — the
 * caller already waited on the liveness probe). A failure is logged, not fatal:
 * the session is still usable and the absent pong will route the user to the
 * AC-3a recovery affordance.
 */
export async function primeAgentChat(
  sessionId: string,
  token: string,
  deps: PrimeDeps = defaultPrimeDeps,
): Promise<void> {
  try {
    await deps.inject(sessionId, buildPrimeLine(sessionId, token), undefined, { gate: "none" });
  } catch (e) {
    logger.warn("agent-chat", `prime failed for ${sessionId}: ${e}`);
  }
}
