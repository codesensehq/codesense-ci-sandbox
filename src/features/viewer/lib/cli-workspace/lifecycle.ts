// SPDX-License-Identifier: MIT
/**
 * OC-4 — CLI workspace lifecycle contract.
 *
 * AC3 requires the eight lifecycle phases (spawn, attach, ready, active, hidden,
 * detached, restored, terminated) to have defined owners, legal transitions,
 * structured errors, and cancellation rules — and, crucially, the *viewport*
 * lifecycle to be a separate axis from the *PTY process* lifecycle. The OC-1 map
 * confirmed the coupling to break: a viewport detach/reattach must not imply a
 * PTY spawn/teardown, and a PTY exit must not depend on any viewport being
 * mounted.
 *
 * So the eight phases are modeled as two disjoint state machines with an
 * ownership map, not one flat enum. Contract-only: transition tables + a pure
 * validator + a structured error. No runtime behavior, no timers, no I/O.
 */

/** The PTY process axis — owned by the Rust terminal runtime (OC-10). The OS
 *  process/session exists independently of whether any viewport is showing it. */
export type PtyLifecyclePhase = "spawn" | "ready" | "active" | "terminated";

/** The viewport axis — owned by the frontend CLI module (OC-9). A viewport is a
 *  transient xterm surface that attaches to, hides from, and detaches from an
 *  already-living PTY session. */
export type ViewportLifecyclePhase =
  | "attach"
  | "ready"
  | "active"
  | "hidden"
  | "detached"
  | "restored"
  | "terminated";

/** Every phase AC3 names, and which axis owns it. `ready`/`active`/`terminated`
 *  are meaningful on both axes; the owner map records where each phase *originates*
 *  as a distinct lifecycle concept. */
export type CliLifecyclePhase = PtyLifecyclePhase | ViewportLifecyclePhase;

export type LifecycleAxis = "pty" | "viewport";

/** Who owns a phase: one axis, or `shared` when the phase is a legitimate state
 *  on both axes and no single axis can claim it. */
export type PhaseOwner = LifecycleAxis | "shared";

/** The owner of each named phase. `spawn` is PTY-only; `attach`, `hidden`,
 *  `detached`, `restored` are viewport-only; `ready`, `active`, and `terminated`
 *  are genuine states on both axes, so they are `shared` rather than arbitrarily
 *  attributed to one — a consumer asking "which axis owns `ready`?" gets an
 *  honest answer instead of a flattened one. */
export const LIFECYCLE_PHASE_OWNER: Record<CliLifecyclePhase, PhaseOwner> = {
  spawn: "pty",
  attach: "viewport",
  hidden: "viewport",
  detached: "viewport",
  restored: "viewport",
  ready: "shared",
  active: "shared",
  terminated: "shared",
};

/** Legal PTY transitions. Teardown (`terminated`) is reachable from any live
 *  phase (kill/exit/cancellation); `terminated` is absorbing. */
export const PTY_TRANSITIONS: Record<PtyLifecyclePhase, readonly PtyLifecyclePhase[]> = {
  spawn: ["ready", "terminated"],
  ready: ["active", "terminated"],
  active: ["ready", "terminated"],
  terminated: [],
};

/** Legal viewport transitions. A viewport attaches to a living PTY, becomes
 *  ready then active, may hide/detach and later restore, and is terminated when
 *  its tab/session closes. None of these touch the PTY axis. */
export const VIEWPORT_TRANSITIONS: Record<
  ViewportLifecyclePhase,
  readonly ViewportLifecyclePhase[]
> = {
  attach: ["ready", "detached", "terminated"],
  ready: ["active", "hidden", "detached", "terminated"],
  active: ["hidden", "detached", "ready", "terminated"],
  hidden: ["restored", "detached", "terminated"],
  detached: ["restored", "terminated"],
  restored: ["ready", "active", "hidden", "terminated"],
  terminated: [],
};

/** Why a transition was rejected — a structured, branchable error (never a bare
 *  throw or a boolean the caller must interpret). */
export interface LifecycleTransitionError {
  readonly code: "illegal-transition" | "cross-axis" | "cancelled";
  readonly axis: LifecycleAxis;
  readonly from: CliLifecyclePhase;
  readonly to: CliLifecyclePhase;
  readonly message: string;
}

function isPhaseOfAxis(axis: LifecycleAxis, phase: CliLifecyclePhase): boolean {
  const table = axis === "pty" ? PTY_TRANSITIONS : VIEWPORT_TRANSITIONS;
  // own-property check, not `in`, so a non-phase string that collides with an
  // inherited `Object.prototype` key (e.g. `"toString"`) cannot masquerade as a
  // valid phase and reach the table lookup below.
  return Object.prototype.hasOwnProperty.call(table, phase);
}

/** Validate a transition on a single axis. Returns `null` on success or a
 *  structured error — the caller decides how to surface it. A `to` phase that
 *  belongs to the other axis is a `cross-axis` error, which is how the contract
 *  enforces viewport/PTY independence: you cannot drive one axis from the other. */
export function validateTransition(
  axis: LifecycleAxis,
  from: CliLifecyclePhase,
  to: CliLifecyclePhase,
): LifecycleTransitionError | null {
  if (!isPhaseOfAxis(axis, from) || !isPhaseOfAxis(axis, to)) {
    return {
      code: "cross-axis",
      axis,
      from,
      to,
      message: `phase ${isPhaseOfAxis(axis, from) ? to : from} is not on the ${axis} axis`,
    };
  }
  const table = axis === "pty" ? PTY_TRANSITIONS : VIEWPORT_TRANSITIONS;
  const legal = (table as Record<string, readonly string[]>)[from];
  if (!legal.includes(to)) {
    return {
      code: "illegal-transition",
      axis,
      from,
      to,
      message: `${axis}: ${from} → ${to} is not a legal transition`,
    };
  }
  return null;
}

export function isTransitionLegal(
  axis: LifecycleAxis,
  from: CliLifecyclePhase,
  to: CliLifecyclePhase,
): boolean {
  return validateTransition(axis, from, to) === null;
}

/** The cancellation rule (AC3): an in-flight transition out of `from` is
 *  cancelled — superseded by a newer activation generation, or aborted before it
 *  committed — so the phase stays at `from`. This yields the `cancelled` error
 *  variant as a first-class, callable outcome rather than a code no one produces,
 *  so OC-5/OC-9 route cancellation through one definition instead of re-inventing
 *  it. `to` echoes `from`: cancellation lands nowhere new. */
export function cancelTransition(
  axis: LifecycleAxis,
  from: CliLifecyclePhase,
): LifecycleTransitionError {
  return {
    code: "cancelled",
    axis,
    from,
    to: from,
    message: `${axis}: transition out of ${from} was cancelled`,
  };
}
