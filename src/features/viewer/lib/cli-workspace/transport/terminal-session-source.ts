// SPDX-License-Identifier: MIT
/**
 * Transport seam — the live PTY session command surface (all backed by
 * `pty.rs`). This is one of the few sanctioned importers of `@/tauri/commands`
 * for the CLI workspace module: I/O, resize and stream subscribe (a
 * terminal is opened and closed through its host's layout, in
 * `terminal-layout-source`). The frontend terminal seam calls these through this
 * module so no component, hook, or lib invokes Tauri directly (NFR5).
 *
 * Pass-through by design: the call guards (disposed refs, generation fences,
 * idle-gates, silent-Ok catches) are caller-specific and stay at the call sites.
 * This module owns only the single import point.
 */

export {
  writePty,
  terminalLaunchActivityLine,
  submitPtyLine,
  resizePty,
  sessionIsIdle,
  sessionsRunningProcesses,
  reserveTerminalStream,
  subscribeTerminal,
  unsubscribeTerminal,
} from "@/tauri/commands";

export type {
  TerminalSnapshot,
  TerminalStreamEvent,
  TerminalSubscription,
  TerminalStreamsLost,
  RunningJobs,
} from "@/tauri/commands";
export { TERMINAL_STREAMS_LOST_EVENT } from "@/tauri/commands";
