// SPDX-License-Identifier: MIT
/**
 * OC-4 — the frontend CLI workspace public capability interface (FR12).
 *
 * This is the single door CodeSense consumes: activation, navigation, focus
 * requests, viewport attachment, terminal session lifecycle, zoom targeting,
 * renderer readiness, and safe diagnostics. Every method signature is expressed
 * purely in contract types (branded identities, intents, lifecycle phases) — no
 * DOM element, React node, Zustand store, xterm handle, Tauri invoke, pipeline,
 * dashboard, or Viewer implementation type appears here. That absence is the
 * FR13/NFR6 guarantee, enforced by `arch-boundary.test.ts`.
 *
 * Contract-only: these are interfaces. OC-9 implements them with adapters; OC-4
 * ships no implementation.
 */

import type {
  ActivationRequest,
  ActiveTerminalTarget,
  NavigationIntent,
  ViewportReadiness,
  WorkspaceEligibility,
  ZoomTarget,
} from "./activation";
import type { SafeActivationDiagnostic } from "./diagnostics";
import type { PtySessionId, StoryWorkspaceId, TerminalTabId, ViewportId } from "./identities";
import type { LifecycleTransitionError, ViewportLifecyclePhase } from "./lifecycle";

/** A rejection that is not a lifecycle transition: activating an ineligible
 *  (hidden/stale/not-yet-attachable) workspace, requesting focus before the
 *  viewport is `ready`, or targeting zoom at a non-visible workspace. Kept
 *  distinct from `LifecycleTransitionError` so a caller can branch on the real
 *  reason rather than seeing every rejection as a bad transition. */
export interface CapabilityRejection {
  readonly code: "ineligible" | "not-ready" | "invalid-target";
  readonly message: string;
}

/** The full error taxonomy a capability call can reject with. */
export type CapabilityError = LifecycleTransitionError | CapabilityRejection;

/** The outcome of a capability call that can be rejected by a typed boundary.
 *  Success carries no payload beyond the discriminant unless a port needs one. */
export type CapabilityResult<T = void> =
  | { readonly ok: true; readonly value: T }
  | { readonly ok: false; readonly error: CapabilityError };

/** Activate a workspace with fully explicit intent (AC2). */
export interface ActivationPort {
  activate(request: ActivationRequest): CapabilityResult;
  /** The workspace's current eligibility (UX-DR5) — a resume must consult this. */
  eligibility(workspace: StoryWorkspaceId): WorkspaceEligibility;
}

/** Move selection along a named axis, including the MRU flip (AC4). */
export interface NavigationPort {
  navigate(intent: NavigationIntent): CapabilityResult<StoryWorkspaceId>;
  /** The most-recently-used eligible workspace an epic resume should land on. */
  mostRecentEligible(): StoryWorkspaceId | null;
}

/** Request input focus for the active terminal without stealing it elsewhere. */
export interface FocusPort {
  requestFocus(target: ActiveTerminalTarget): CapabilityResult;
  activeTarget(): ActiveTerminalTarget | null;
}

/** Attach/detach a transient viewport to a living PTY session — the viewport
 *  lifecycle axis (AC3), independent of the PTY process. */
export interface ViewportAttachmentPort {
  attach(session: PtySessionId): CapabilityResult<ViewportId>;
  transition(viewport: ViewportId, to: ViewportLifecyclePhase): CapabilityResult;
  readiness(viewport: ViewportId): ViewportReadiness;
}

/** The terminal session lifecycle — the PTY process axis (AC3), owned by the
 *  Rust runtime and surfaced here only as intent. */
export interface TerminalSessionLifecyclePort {
  session(tab: TerminalTabId): PtySessionId | null;
  close(session: PtySessionId): CapabilityResult;
}

/** Zoom targeting that follows workspace selection (AC4 / UX-DR11). */
export interface ZoomPort {
  zoomTarget(): ZoomTarget;
  setZoomTarget(target: ZoomTarget): CapabilityResult;
}

/** Emit-free access to the safe correlated diagnostic (FR16). Returns the
 *  redacted record; whether/where it is logged is the adapter's decision. */
export interface SafeDiagnosticsPort {
  snapshot(viewport: ViewportId): SafeActivationDiagnostic;
}

/** The whole public capability — the one entry point OC-9 exposes and CodeSense
 *  consumes. Composed from the narrow ports so a consumer depends only on the
 *  slice it uses. */
export interface CliWorkspaceCapability {
  readonly activation: ActivationPort;
  readonly navigation: NavigationPort;
  readonly focus: FocusPort;
  readonly viewports: ViewportAttachmentPort;
  readonly sessions: TerminalSessionLifecyclePort;
  readonly zoom: ZoomPort;
  readonly diagnostics: SafeDiagnosticsPort;
}
