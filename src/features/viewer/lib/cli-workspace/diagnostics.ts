// SPDX-License-Identifier: MIT
/**
 * OC-4 — FR16 safe CLI workspace diagnostics schema.
 *
 * The contract owns the *shape* and the *redaction rule* of the correlated
 * activation record; OC-11 owns the exhaustive redaction test that consolidates
 * against it. This module defines the safe field set only — it wires up NO
 * emission.
 *
 * The redaction guarantee is structural: `SafeActivationDiagnostic` has no field
 * that could hold terminal input, output, command text, cwd, environment, or
 * credentials. It carries only identities, intent, visibility, lifecycle, and the
 * integer sequence watermarks — the same posture as the connector-account
 * snapshot on the Rust side (secrets scrubbed at construction).
 */

import type { FocusIntent, Presence, RendererReadiness } from "./activation";
import { identityValue } from "./identities";
import type {
  EpicRailId,
  PtySessionId,
  StoryWorkspaceId,
  TeamId,
  TerminalTabId,
  ViewportId,
} from "./identities";

/** Whether the host surface is currently presenting the viewport — the
 *  visibility axis the rAF/first-presentation work (OC-2) turns on. */
export type HostVisibility = "visible" | "hidden" | "occluded";

/**
 * The safe correlated activation record. Every field is a correlation key, an
 * intent/state discriminant, or an integer watermark — deliberately never
 * content. Identities are stored as their raw wire strings so the record is a
 * plain serializable DTO.
 */
export interface SafeActivationDiagnostic {
  readonly teamId: string;
  readonly epicRailId: string;
  readonly workspaceId: string;
  readonly terminalTabId: string;
  readonly ptySessionId: string;
  readonly viewportId: string | null;
  readonly focusIntent: FocusIntent;
  readonly presence: Presence;
  readonly hostVisibility: HostVisibility;
  readonly rendererReadiness: RendererReadiness;
  /** Monotonic activation stamp — lets a trace reader order records and spot a
   *  stale callback (NFR14). */
  readonly generation: number;
  /** Absolute committed byte position the viewport had received (never content). */
  readonly committedHead: number;
  /** Absolute byte position actually rendered (never content). */
  readonly renderedHead: number;
}

/** The identity inputs, typed, that a caller supplies to build a record. Kept
 *  separate from the flattened DTO so the branded identities guard the call site
 *  while the emitted record stays a plain serializable shape. */
export interface SafeActivationDiagnosticInput {
  readonly team: TeamId;
  readonly epicRail: EpicRailId;
  readonly workspace: StoryWorkspaceId;
  readonly terminalTab: TerminalTabId;
  readonly ptySession: PtySessionId;
  readonly viewport: ViewportId | null;
  readonly focusIntent: FocusIntent;
  readonly presence: Presence;
  readonly hostVisibility: HostVisibility;
  readonly rendererReadiness: RendererReadiness;
  readonly generation: number;
  readonly committedHead: number;
  readonly renderedHead: number;
}

/** Build the redacted record from typed identities. Pure; performs no I/O and
 *  emits nothing — a caller (OC-9's adapter) decides whether to log it. */
export function buildSafeActivationDiagnostic(
  input: SafeActivationDiagnosticInput,
): SafeActivationDiagnostic {
  return {
    teamId: identityValue(input.team),
    epicRailId: identityValue(input.epicRail),
    workspaceId: identityValue(input.workspace),
    terminalTabId: identityValue(input.terminalTab),
    ptySessionId: identityValue(input.ptySession),
    viewportId: input.viewport === null ? null : identityValue(input.viewport),
    focusIntent: input.focusIntent,
    presence: input.presence,
    hostVisibility: input.hostVisibility,
    rendererReadiness: input.rendererReadiness,
    generation: input.generation,
    committedHead: input.committedHead,
    renderedHead: input.renderedHead,
  };
}
