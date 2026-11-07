// SPDX-License-Identifier: MIT
import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";

import type { AgentWorkspaceConfig } from "@/shared/lib/run-agent-bind";
import type { AgentWorkspaceSessionApi } from "@/shared/hooks/use-agent-workspace-session";
import type { AskUserRequest, AskUserResponse } from "@/shared/lib/managed-session";

/** SW-GS-12 (§9) — the reusable pane-visibility policy for a spawned agent workspace. */
export type PaneVisibility = "minimized" | "hidden";

/**
 * SW-GS-12 (§9) — should a connect adapter register a tray-chip workspace record for
 * this visibility? `"hidden"` skips it (PTY-only, surfaced later); anything else (incl.
 * the default) registers the born-minimized chip. Pure so consumers branch identically.
 */
export function shouldRegisterPaneChip(visibility?: PaneVisibility): boolean {
  return visibility !== "hidden";
}

/**
 * SW-AW-5 — the config a feature passes to the reusable right-pane agent module.
 *
 * The module owns the spawn → mount → chrome stack; the consumer supplies only
 * this config. NO agent/vendor literal lives here — `session` carries the caller's
 * cwd (inside its `connect` closure), `targetWorkspace`, and agent basename, so the
 * module is agent- and feature-agnostic (each consumer passes its own dir).
 */
export interface AgentModuleConfig {
  /** Right-pane panel id — stable, persisted in the layout. Never change it once shipped. */
  panelId: string;
  /** Panel title (the View → Panels label + the section tab). */
  title: string;
  /** Lucide panel icon (no unicode glyphs). */
  icon: LucideIcon;
  /**
   * The warm-session config (AC-1/AC-7). `session.targetWorkspace` is the binding
   * `surfaceKey` AND the sole single-session sharing key: two consumers that pass
   * the same `targetWorkspace` land on ONE bound session (AC-7). `autoConnect` and
   * the born-minimized `connect` adapter live here too.
   */
  session: AgentWorkspaceConfig;
  /**
   * SW-GS-12 (§9) — WHEN the agent spawns, as a reusable code-level policy (not a
   * per-feature hack). Read by `<AgentModuleHost>`:
   * - `"lazy"` (default): never spawn unprompted — the host stays dormant until its
   *   `active` signal goes true (or a controlled engage). Today's behavior.
   * - `"onHostMount"`: engage/auto-connect the moment the host is `active` (or, when
   *   uncontrolled, the moment it mounts). The consumer's mount location / `active`
   *   wiring defines the trigger.
   * Note: `"onHostMount"` spawns a real (billed) agent session as soon as its trigger
   * fires; a low-traffic consumer should keep `"lazy"` (or pair with `paneVisibility`).
   */
  prewarm?: "lazy" | "onHostMount";
  /**
   * SW-GS-12 (§9) — WHERE the spawned CLI workspace surfaces, as a reusable policy.
   * Honored by the consumer's `connect` adapter (the module only carries the intent):
   * - `"minimized"` (default): born-minimized workspace record → a chip in the tray,
   *   discoverable via "Open terminal" / `promote()`. Today's behavior.
   * - `"hidden"`: spawn + connect with NO tray chip (the pre-SW-GS-2d primitive — the
   *   PTY rides the tab bucket, independent of any workspace record), surfaced later
   *   on demand. For a headless-until-needed background agent.
   */
  paneVisibility?: PaneVisibility;
  /**
   * "Open terminal" affordance (AC-5): reveal the born-minimized CLI workspace tab
   * so the user can watch/recover the underlying session. Consumer-supplied so the
   * module stays agent-agnostic. Omit to hide the affordance.
   */
  onOpenTerminal?: () => void;
  /**
   * AC-5 action-card slot: read-only, plain-language agent-step rows the consumer
   * renders (the module owns the *shell*; the consumer supplies the *card content*).
   * Receives the bound session id so cards can key off the live session. Distinct
   * from the module-owned answerable-prompt card (`<ManagedQuestionPanel>`).
   */
  actionCards?: (ctx: { boundSessionId: string | null }) => ReactNode;
  /**
   * SW-AW-7 — optional consumer override for the module's answerable-prompt card.
   * A render-prop that receives the bound session's oldest pending `ask_user`
   * request (guaranteed non-null — the module only calls it while an ask is
   * pending) plus an `answer` callback PRE-BOUND to that ask's `correlationId`.
   * The consumer supplies only the *markup* (e.g. a conflict redline +
   * "Keep mine / Keep theirs / Both" choices); the module owns the ask selection,
   * the empty-state guard, and the answer wiring.
   *
   * Build `response` with the exported `acceptAnswer`/`declineAnswer`/`cancelAnswer`/
   * `handledAnswer` builders — `answer` routes through the SAME `answerAskUser` channel the default
   * `<ManagedQuestionPanel>` uses (single answer path, no re-wire, non-blocking).
   * `pendingCount` is the total pending asks for the session (≥ 1), so the consumer
   * can surface a "+N more" backlog hint at parity with the default card. When
   * omitted, the module renders the default `<ManagedQuestionPanel>` unchanged.
   *
   * Robustness: `ask_user` is a BLOCKING request, so the module wraps the slot in an
   * error boundary — if the consumer render THROWS, the module degrades to the
   * default `<ManagedQuestionPanel>` so the human can still answer (the session never
   * silently wedges). A consumer that deliberately returns `null` renders nothing —
   * it owns the obligation to eventually render an answer path for the pending ask.
   * Distinct from `actionCards` (read-only progress rows) — kept separately typed.
   */
  answerablePrompts?: (ctx: {
    ask: AskUserRequest;
    answer: (response: AskUserResponse) => void;
    pendingCount: number;
    sessionId: string;
  }) => ReactNode;
}

/**
 * SW-AW-5 (AC-4a) — the handle `useAgentModule` returns. Load-bearing for the
 * "thin consumer" claim (AC-9): a headless consumer (GS-12) reads `session` with
 * NO panel mounted; a panel consumer (GS-10) calls `promote()` to surface the SAME
 * warm session as a right-pane panel on demand.
 */
export interface AgentModuleApi {
  /** The bound session (the `useAgentWorkspaceSession` api) — reachable outside any panel. */
  session: AgentWorkspaceSessionApi;
  /** True while the panel is NOT currently shown as an expanded right-pane section. */
  isMinimized: boolean;
  /**
   * Surface the panel as the top right-pane section (AC-8), against the already-warm
   * session — adds NO agent spawn. No-op-safe when the panel id isn't registered (a
   * headless-only consumer never mounts the panel, so its `promote()` never leaves
   * an orphan empty section).
   */
  promote: () => void;
  /** Remove the panel from the right-pane layout (reverses `promote`); the session
   *  stays warm/bound — only the visible panel is dropped. */
  minimize: () => void;
}
