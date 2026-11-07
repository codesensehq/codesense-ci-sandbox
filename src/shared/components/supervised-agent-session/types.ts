// SPDX-License-Identifier: MIT
/**
 * Core types for the reusable supervised-agent-session primitive
 * (architecture Rev-2026-06-23 §D). These live HERE (in the generic `shared/`
 * primitive), NOT in any feature — the primitive is kind-agnostic. Features
 * re-export these for their own view-models.
 */

/** Who holds the keyboard. Exactly one owner at any time (AC-10). */
export type SessionOwner = "agent" | "user";

/**
 * Connection phase — THREE-phase, NOT a boolean (AC-14), orthogonal to
 * `owner`. The transitional `reconnecting` is the async relaunch window
 * (AC-13): the chip must NOT claim "Agent driving" until `live`. A
 * dead/cleared session is `disconnected` regardless of intent.
 */
export type ConnectionPhase = "live" | "disconnected" | "reconnecting";

/** The supervised-session ownership/connection state (AC-10/14). */
export interface AgentSessionState {
  owner: SessionOwner;
  connection: ConnectionPhase;
}
