// SPDX-License-Identifier: MIT
/**
 * The canonical terminal-AI status vocabulary — the single source of truth for
 * the dot's literal set. Every surface that paints an agent-status dot (tab
 * pill, overflow "+N", tile roll-up, tray chip, minimized pill, board card)
 * renders one of these, projected from the session's agent-state record
 * (`agent-state.ts`) or, for a session without one, from the stored status.
 * Adding a status variant is a one-place edit here plus its swatch.
 *
 * Lives in its own framework-free module (not the viewer store) so both the type
 * and the runtime array have one home and neither can be mocked away by a partial
 * store mock.
 */
export const TERMINAL_AI_STATUSES = [
  "running",
  "thinking",
  "done",
  "error",
  "crashed",
  "needs-permission",
] as const;

export type TerminalAiStatus = (typeof TERMINAL_AI_STATUSES)[number];

/**
 * The coarser subset the `terminal-status-update` wire carries (the Rust
 * receiver's `terminal_dot_wire`) and the stored per-session status holds: a
 * model turn reads `running` and any failure `error` there. The finer
 * `thinking` / `crashed` exist only in the agent-state record, so they reach
 * the dot through its projection, never through the stored slot. The wire
 * additionally accepts a transient `"idle"` signal (which clears the stored
 * status rather than being stored).
 */
export const TERMINAL_WIRE_STATUSES = [
  "running",
  "done",
  "error",
  "needs-permission",
] as const satisfies readonly TerminalAiStatus[];

export type TerminalWireStatus = (typeof TERMINAL_WIRE_STATUSES)[number];

/**
 * Why a session needs you, in rank order: a pending question, a crash, a CLI
 * stuck on a startup screen it never reported ready from, a declared failure,
 * the agent's own `signal_done(needs-attention)`, a
 * self-reported `signal_progress {phase:"blocked"}`, a permission prompt. The
 * tabs, the dock badge, the notifications and the agents list all read this
 * one vocabulary. `blocked` is distinct from `permission` on purpose: a
 * `PermissionRequest` is an approval gate, while `signal_progress blocked` is
 * the agent itself saying "I am held, with a reason I will show in the note".
 */
export type NeedsYouKind =
  | "question"
  | "crashed"
  | "failed"
  | "attention"
  | "startup"
  | "permission"
  | "blocked";
