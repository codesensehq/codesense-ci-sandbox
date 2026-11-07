// SPDX-License-Identifier: MIT
import {
  awaitPong,
  type AgentMessageSubscribe,
  type HandshakeTimer,
} from "@/shared/lib/agent-handshake";
import { newHandshakeToken, primeAgentChat } from "@/shared/lib/agent-chat-prime";
import { pressEnter } from "@/shared/lib/agent-session-injection";

/**
 * SW-GS-24 — the standalone, reusable "is this agent actually live AND primed right
 * now" primitive: mint a one-shot token, prime the session (the ping), and await the
 * pong on `mcp:agent-message`, nudging a discrete Enter until it lands or the bounded
 * timeout fires. Resolves `true` on a proven round-trip, `false` otherwise.
 *
 * This was previously a closure (`primeAndHandshake`) inside `use-agent-workspace-session`.
 * It is extracted here so a SECOND caller — the headless git-sync draft path
 * (`use-outbox` / `draft-agent-text`), which deliberately does NOT mount that hook
 * (§9 single-mount) — can re-verify liveness immediately before injecting a draft,
 * instead of trusting the stale bind-time `connection === "live"` snapshot. One
 * liveness definition, reused by both the bind path and the draft path — no drift.
 *
 * Pure composition over the existing `awaitPong` + prime + nudge seams; every effectful
 * dependency is injectable so it unit-tests without a PTY or the chat store.
 */

/** How long to wait for the agent's pong before treating it as not-live. Generous —
 *  the agent must read the prime and make an MCP call (LLM inference) — but bounded so
 *  a dead/drifted session is caught in ~this-many ms, NOT the 90s draft budget. */
export const HANDSHAKE_TIMEOUT_MS = 15_000;

/** Cadence for the Enter-nudge during the handshake: if the prime landed but the agent
 *  batched it as a paste (held the Enter), a discrete Enter every this-many ms submits
 *  it, until the pong confirms (or the handshake times out). */
export const ENTER_NUDGE_MS = 1_500;

export interface VerifyAgentLiveOptions {
  /** Pong budget (default `HANDSHAKE_TIMEOUT_MS`). */
  timeoutMs?: number;
  /** Enter-nudge cadence (default `ENTER_NUDGE_MS`). */
  nudgeMs?: number;
  /** Event-channel registration seam (default: real `mcp:agent-message`). */
  subscribe?: AgentMessageSubscribe;
  /** Timer seam (default: real setTimeout). */
  timer?: HandshakeTimer;
  /** Prime injection seam (default: `primeAgentChat`). */
  prime?: (sessionId: string, token: string) => Promise<void>;
  /** Enter-nudge seam (default: `pressEnter`). */
  nudge?: (sessionId: string) => void | Promise<void>;
  /** Token minter seam (default: `newHandshakeToken`). */
  mintToken?: () => string;
}

/**
 * Resolve `true` when the agent proves it's live by ponging our one-shot token within
 * `timeoutMs`; otherwise `false`. Re-primes the session as the ping, so a call that
 * succeeds ALSO re-establishes the "reply via post_agent_message" instruction — self
 * healing against prime drift. Always tears down the listener + timers (via `awaitPong`).
 */
export function verifyAgentLive(
  sessionId: string,
  opts: VerifyAgentLiveOptions = {},
): Promise<boolean> {
  const mintToken = opts.mintToken ?? newHandshakeToken;
  const prime = opts.prime ?? primeAgentChat;
  const nudge = opts.nudge ?? pressEnter;
  const token = mintToken();
  return awaitPong({
    sessionId,
    token,
    timeoutMs: opts.timeoutMs ?? HANDSHAKE_TIMEOUT_MS,
    subscribe: opts.subscribe,
    timer: opts.timer,
    onListening: () => prime(sessionId, token),
    onNudge: () => nudge(sessionId),
    nudgeMs: opts.nudgeMs ?? ENTER_NUDGE_MS,
  });
}
