// SPDX-License-Identifier: MIT
import type { ConnectionPhase, SessionOwner } from "./types";

/**
 * SW-GS-1 — the reusable "supervised agent session" ownership primitive
 * (architecture Rev-2026-06-23 §D). GENERIC and kind-agnostic: parameterized
 * ONLY by a bound session + reconnect/disconnect callbacks, so SW-DR and
 * future agentic-driving over story/epic workspaces adopt it unchanged. The
 * binding is the only task-specific wiring.
 *
 * CONTROLLED by design — the consumer owns `state` (`{ owner, connection }`)
 * and the callbacks mutate it. This keeps feature-wide gating possible
 * (chat box / V3-V4 approval live OUTSIDE the primitive's subtree and read the
 * same store) and makes the hook trivially testable (a test supplies
 * `useState`).
 */

/** The session this primitive supervises. Consumer-owned state + callbacks. */
export interface AgentSessionBinding {
  /** Bound session's PTY id (the role-tagged tab's sessionId). Null pre-spawn. */
  sessionId: string | null;
  /** Who holds the keyboard. */
  owner: SessionOwner;
  /** Three-phase connection (AC-14), orthogonal to `owner`. */
  connection: ConnectionPhase;
  /**
   * Take the wheel (AC-12): relinquish agent control so the terminal becomes
   * user-interactive. The session keeps running; the host flips `owner` →
   * "user". Mechanics are SW-GS-2a; here it just records intent.
   */
  onDisconnect: () => void;
  /**
   * Hand back (AC-13): ask the host to relaunch a FRESH agent context (no
   * --resume) + auto `/cs-status`. The host drives `connection`
   * "reconnecting" → "live" and flips `owner` → "agent". Mechanics are
   * SW-GS-2a.
   */
  onReconnect: () => void;
}

export interface AgentSessionOwnershipApi {
  owner: SessionOwner;
  connection: ConnectionPhase;
  /**
   * Terminal input gate (AC-11/12): the agent holds the keyboard ⇒ user input
   * must NOT reach the PTY. Locked whenever `owner === "agent"`; released the
   * instant the user takes control. Independent of `connection` — locking an
   * already-dead session is harmless, and we never want a window where the
   * agent "owns" but input leaks.
   */
  inputLocked: boolean;
  /**
   * Chip shows "Agent driving" ONLY when this is true (AC-10/14): the agent
   * owns AND the session is live. During `reconnecting` it is false (the chip
   * shows the transitional state, never "driving" before the session exists).
   */
  isDriving: boolean;
  /**
   * A live bound session exists ⇒ chat (AC-18) + V3/V4 approvals (AC-16) are
   * enabled. When false they are gated with a "reconnect to continue" reason
   * (AC-15). Read-only V1/V2 stay live regardless (they read the session-less
   * publish-set source).
   */
  hasLiveSession: boolean;
  disconnect: () => void;
  reconnect: () => void;
}

/** Derive the supervised-session control surface from a binding. Pure given
 *  the binding — no internal state, so the consumer's store is the single
 *  source of truth.
 *
 *  SW-WV-A1 — this hook is PANE-SCOPED and orthogonal to the scope-level Weft
 *  lease (`wv_lease`). The two authority axes are independent (spine §2.3):
 *  - scope-lease (`wv_lease(scope)`) = who ORCHESTRATES the fleet, and
 *  - pane-ownership (`owner: agent|user`, here) = who STEERS this one worker.
 *  Holding the root lease does NOT grant `owner === "agent"` on any pane; a
 *  human who takes the wheel of a worker (`owner === "user"`) keeps
 *  `inputLocked === true` for that pane even while the controller holds the
 *  fleet lease. Acquiring or losing the lease never touches this surface. */
export function useAgentSessionOwnership(binding: AgentSessionBinding): AgentSessionOwnershipApi {
  const { sessionId, owner, connection, onDisconnect, onReconnect } = binding;
  return {
    owner,
    connection,
    inputLocked: owner === "agent",
    isDriving: owner === "agent" && connection === "live",
    hasLiveSession: sessionId !== null && connection === "live",
    disconnect: onDisconnect,
    reconnect: onReconnect,
  };
}
