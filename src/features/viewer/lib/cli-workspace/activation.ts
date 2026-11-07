// SPDX-License-Identifier: MIT
/**
 * OC-4 — activation, navigation, focus, zoom, and readiness concepts.
 *
 * These are the domain-selection values that cross the public capability
 * interface. AC2 requires an activation request to carry explicit foreground /
 * background / focus intent, and the selection type to be independent of DOM,
 * React, Zustand, xterm, and Tauri. AC4 requires the surface to name MRU,
 * eligibility, zoom targeting, and renderer readiness as first-class concepts so
 * OC-5/OC-6/OC-7/OC-8 bind to one definition instead of re-deriving them from the
 * viewer store.
 *
 * Contract-only. Every type here is a plain value; nothing imports a framework.
 */

import type {
  DomFocusId,
  ForegroundTileId,
  RendererId,
  StoryWorkspaceId,
  TerminalTabId,
} from "./identities";

/** Whether the activated workspace should take the foreground tile or warm in
 *  the background. Explicit so a background activation can never accidentally
 *  steal the foreground (UX-DR2). */
export type Presence = "foreground" | "background";

/** What the activation intends to do with input focus — a first-class concept so
 *  focus is one decision spanning selection through input (the OC-1 finding that
 *  focus intent had no single owner). `preserve-focus` leaves focus where it is
 *  (e.g. a background warm); `focus-elsewhere` explicitly hands focus to a
 *  non-terminal target (double-click-to-open, controls) so it is never a
 *  terminal-focus by omission. */
export type FocusIntent = "focus-terminal" | "preserve-focus" | "focus-elsewhere";

/** A request to activate a workspace across the public interface. Carries the
 *  full intent explicitly — no ambient/global state is consulted to infer it.
 *  `generation` is the monotonic activation stamp a stale callback is rejected
 *  against (NFR14); it is a plain number, not a store subscription. */
export interface ActivationRequest {
  readonly workspace: StoryWorkspaceId;
  readonly tile: ForegroundTileId;
  readonly presence: Presence;
  readonly focus: FocusIntent;
  readonly generation: number;
}

/** The navigation dimensions the epic/workspace/tab/pane/MRU model exposes. AC4
 *  requires MRU to be a named axis, not folded into a generic "navigation", so
 *  OC-6/OC-7 can distinguish tab cycling from workspace cycling from an MRU flip
 *  (UX-DR6). */
export type NavigationAxis = "epic-rail" | "story-workspace" | "terminal-tab" | "pane" | "mru";

export type NavigationDirection = "previous" | "next";

export interface NavigationIntent {
  readonly axis: NavigationAxis;
  readonly direction: NavigationDirection;
}

/** Whether a workspace can currently be activated, and if not, why. AC4/UX-DR5
 *  treat eligibility as a first-class primitive: an MRU resume must pick an
 *  *eligible* workspace, and a hidden/stale/not-yet-attachable target must be
 *  represented honestly rather than activated blindly. */
export type WorkspaceEligibility = "eligible" | "hidden" | "stale" | "not-yet-attachable";

/** The zoom target a workspace switch must follow. AC4/UX-DR11: switching
 *  workspaces while zoomed shows the newly selected workspace in the same
 *  expanded region — so the contract carries "which workspace zoom points at",
 *  not a boolean the viewport layer re-derives. `none` = not zoomed. */
export type ZoomTarget =
  | { readonly kind: "none" }
  | { readonly kind: "workspace"; readonly workspace: StoryWorkspaceId };

/** The observable readiness of a viewport's renderer, driven by real signals
 *  (geometry, font, renderer, attachment) rather than a delay (NFR14). A focus
 *  request is gated on `ready`. */
export type RendererReadiness = "geometry-pending" | "font-pending" | "renderer-pending" | "ready";

/** A viewport's readiness as a discriminated union so illegal combinations are
 *  unrepresentable (the branded-identity thesis applied to readiness): the
 *  renderer identity exists *only* when `ready`. A caller cannot construct
 *  `{ readiness: "geometry-pending", renderer }` — the state the prose said was
 *  impossible is now impossible by type. */
export type ViewportReadiness =
  | {
      readonly readiness: "ready";
      readonly renderer: RendererId;
    }
  | { readonly readiness: Exclude<RendererReadiness, "ready"> };

/** The identity of the terminal tab an activation ultimately routes input to,
 *  paired with its owning workspace and the DOM focus target the cascade claims —
 *  the join OC-5's focus cascade needs. */
export interface ActiveTerminalTarget {
  readonly workspace: StoryWorkspaceId;
  readonly terminalTab: TerminalTabId;
  readonly domFocus: DomFocusId;
}

/** NFR14 stale-activation rule as one callable primitive: an incoming callback's
 *  generation `supersedes` the current one only when it is newer. A stale callback
 *  (`incoming <= current`) must not mutate a newer activation. OC-5/OC-6 route the
 *  compare through this instead of each re-implementing `<`. */
export function supersedes(current: number, incoming: number): boolean {
  return incoming > current;
}

/** A background activation that also asks to focus the terminal is contradictory
 *  (UX-DR2: a background warm must not steal the foreground/focus). The two fields
 *  are kept independent for expressiveness; this rule names the one combination
 *  the contract forbids so consumers don't each re-derive it. */
export function activationIsCoherent(request: ActivationRequest): boolean {
  return !(request.presence === "background" && request.focus === "focus-terminal");
}
